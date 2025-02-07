// Copyright 2019 Normation SAS
//
// This file is part of Rudder.
//
// Rudder is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In accordance with the terms of section 7 (7. Additional Terms.) of
// the GNU General Public License version 3, the copyright holders add
// the following Additional permissions:
// Notwithstanding to the terms of section 5 (5. Conveying Modified Source
// Versions) and 6 (6. Conveying Non-Source Forms.) of the GNU General
// Public License version 3, when you create a Related Module, this
// Related Module is not considered as a part of the work and may be
// distributed under the license agreement of your choice.
// A "Related Module" means a set of sources files including their
// documentation that, without modification of the Source Code, enables
// supplementary functions or services in addition to those offered by
// the Software.
//
// Rudder is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Rudder.  If not, see <http://www.gnu.org/licenses/>.

use crate::error::Error;
use chrono::prelude::DateTime;
use chrono::{Duration, Utc};
use hyper::StatusCode;
use openssl::error::ErrorStack;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::pkey::Public;
use openssl::rsa::Rsa;
use openssl::sign::Verifier;
use regex::Regex;
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;

pub enum HashType {
    Sha256,
    Sha512,
}

impl FromStr for HashType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sha256" => Ok(HashType::Sha256),
            "sha512" => Ok(HashType::Sha512),
            _ => Err(Error::InvalidHashType),
        }
    }
}

impl HashType {
    fn hash(&self, bytes: &[u8]) -> String {
        match &self {
            HashType::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.input(bytes);
                format!("{:x}", hasher.result())
            }
            HashType::Sha512 => {
                let mut hasher = Sha512::new();
                hasher.input(bytes);
                format!("{:x}", hasher.result())
            }
        }
    }

    fn to_openssl_hash(&self) -> MessageDigest {
        match &self {
            HashType::Sha256 => MessageDigest::sha256(),
            HashType::Sha512 => MessageDigest::sha512(),
        }
    }
}

#[derive(Debug)]
pub struct Metadata {
    header: String,
    algorithm: String,
    digest: String,
    hash_value: String,
    short_pubkey: String,
    hostname: String,
    keydate: String,
    keyid: String,
    expires: String,
}

impl FromStr for Metadata {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Metadata {
            header: parse_value("header", s).unwrap(),
            algorithm: parse_value("algorithm", s).unwrap(),
            digest: parse_value("digest", s).unwrap(),
            hash_value: parse_value("hash_value", s).unwrap(),
            short_pubkey: parse_value("short_pubkey", s).unwrap(),
            hostname: parse_value("hostname", s).unwrap(),
            keydate: parse_value("keydate", s).unwrap(),
            keyid: parse_value("keyid", s).unwrap(),
            expires: parse_value("expires", s).unwrap(),
        })
    }
}

impl Metadata {
    pub fn new(hashmap: HashMap<String, String>) -> Result<Self, Error> {
        Ok(Metadata {
            header: hashmap.get("header").unwrap().to_string(),
            algorithm: hashmap.get("algorithm").unwrap().to_string(),
            digest: hashmap.get("digest").unwrap().to_string(),
            hash_value: hashmap.get("hash_value").unwrap().to_string(),
            short_pubkey: hashmap.get("short_pubkey").unwrap().to_string(),
            hostname: hashmap.get("hostname").unwrap().to_string(),
            keydate: hashmap.get("keydate").unwrap().to_string(),
            keyid: hashmap.get("keyid").unwrap().to_string(),
            expires: hashmap.get("expires").unwrap().to_string(),
        })
    }
}

impl fmt::Display for Metadata {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut mystring = String::new();

        mystring.push_str(&format!("header={}\n", &self.header));
        mystring.push_str(&format!("algorithm={}\n", &self.algorithm));
        mystring.push_str(&format!("digest={}\n", &self.digest));
        mystring.push_str(&format!("hash_value={}\n", &self.hash_value));
        mystring.push_str(&format!("short_pubkey={}\n", &self.short_pubkey));
        mystring.push_str(&format!("hostname={}\n", &self.hostname));
        mystring.push_str(&format!("keydate={}\n", &self.keydate));
        mystring.push_str(&format!("keyid={}\n", &self.keyid));
        mystring.push_str(&format!(
            "expires={}\n",
            parse_ttl(self.expires.clone().to_string()).unwrap()
        ));
        write!(f, "{}", mystring)
    }
}

pub fn parse_value(key: &str, file: &str) -> Result<String, ()> {
    let regex_key = Regex::new(&format!(r"{}=(?P<key>[^\n]+)\n", key)).unwrap();

    match regex_key.captures(&file) {
        Some(y) => match y.name("key") {
            Some(x) => Ok(x.as_str().parse::<String>().unwrap()),
            _ => Err(()),
        },
        None => Err(()),
    }
}

pub fn same_hash_than_in_nodeslist(
    pubkey: PKey<Public>,
    hash_type: HashType,
    keyhash: &str,
) -> Result<bool, Error> {
    let public_key_der: &[u8] = &pubkey.public_key_to_der()?;

    Ok(hash_type.hash(public_key_der) == keyhash)
}

pub fn get_pubkey(pubkey: String) -> Result<PKey<Public>, ErrorStack> {
    PKey::from_rsa(Rsa::public_key_from_pem_pkcs1(
        format!(
            "-----BEGIN RSA PUBLIC KEY-----\n{}\n-----END RSA PUBLIC KEY-----\n",
            pubkey
        )
        .as_bytes(),
    )?)
}

pub fn validate_signature(
    file: &[u8],
    pubkey: PKey<Public>,
    hash_type: HashType,
    digest: &[u8],
) -> Result<bool, ErrorStack> {
    let mut verifier = Verifier::new(hash_type.to_openssl_hash(), &pubkey).unwrap();
    verifier.update(file).unwrap();
    verifier.verify(digest)
}

pub fn parse_ttl(ttl: String) -> Result<i64, Error> {
    let regex_numbers = Regex::new(r"^(?:(?P<days>\d+)(?:d|days))?\s*(?:(?P<hours>\d+)(?:h|hours))?\s*(?:(?P<minutes>\d+)(?:m|minutes))?\s*(?:(?P<seconds>\d+)(?:s|seconds))?").unwrap();

    fn parse_time<'t>(cap: &regex::Captures<'t>, n: &str) -> Result<i64, Error> {
        Ok(match cap.name(n) {
            Some(s) => s.as_str().parse::<i64>()?,
            None => 0,
        })
    }

    if let Some(cap) = regex_numbers.captures(&ttl) {
        let s = parse_time(&cap, "seconds")?;
        let m = parse_time(&cap, "minutes")?;
        let h = parse_time(&cap, "hours")?;
        let d = parse_time(&cap, "days")?;

        let expiration: DateTime<Utc> = Utc::now()
            + Duration::seconds(s)
            + Duration::seconds(m)
            + Duration::seconds(h)
            + Duration::seconds(d);

        Ok(expiration.timestamp())
    } else {
        Err(Error::InvalidTtl(ttl))
    }
}

pub fn metadata_writer(metadata_string: String, peek: &str) {
    let myvec: Vec<String> = peek.split('/').map(|s| s.to_string()).collect();
    let (target_uuid, source_uuid, _file_id) = (&myvec[0], &myvec[1], &myvec[2]);
    let _ = fs::create_dir_all(format!("./{}/{}/", target_uuid, source_uuid)); // on cree les folders s'ils existent pas
                                                                               //fs::create_dir_all(format!("/var/rudder/configuration-repository/shared-files/{}/{}/", target_uuid, source_uuid)); // real path
    fs::write(format!("./{}", peek), metadata_string).expect("Unable to write file");
}

pub fn metadata_hash_checker(filename: String, hash: String) -> hyper::StatusCode {
    if !Path::new(&filename).exists() {
        return StatusCode::from_u16(404).unwrap();
    }

    let contents = fs::read_to_string(&filename).expect("Something went wrong reading the file");

    let re = Regex::new(r"hash_value=([a-f0-9]+)\n").expect("unable to parse hash regex");

    if re
        .captures_iter(&contents)
        .next()
        .and_then(|s| s.get(1))
        .map(|s| s.as_str())
        == Some(&hash)
    {
        return StatusCode::from_u16(200).unwrap();
    }

    StatusCode::from_u16(404).unwrap()
}

pub fn parse_hash_from_raw(raw: String) -> String {
    raw.split('=')
        .map(|s| s.to_string())
        .filter(|s| s != "hash")
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::sign::Signer;

    #[test]
    pub fn it_writes_the_metadata() {
        let metadata = Metadata {
            header: "rudder-signature-v1".to_string(),
            algorithm: "sha256".to_string(),
            digest: "8ca9efc5752e133e2e80e2661c176fa50f".to_string(),
            hash_value: "a75fda39a7af33eb93ab1c74874dcf66d5761ad30977368cf0c4788cf5bfd34f"
                .to_string(),
            short_pubkey: "shortpubkey".to_string(),
            hostname: "ubuntu-18-04-64".to_string(),
            keydate: "2018-10-3118:21:43.653257143".to_string(),
            keyid: "B29D02BB".to_string(),
            expires: "1d 1h".to_string(),
        };

        assert_eq!(format!("{}", metadata), format!("header=rudder-signature-v1\nalgorithm=sha256\ndigest=8ca9efc5752e133e2e80e2661c176fa50f\nhash_value=a75fda39a7af33eb93ab1c74874dcf66d5761ad30977368cf0c4788cf5bfd34f\nshort_pubkey=shortpubkey\nhostname=ubuntu-18-04-64\nkeydate=2018-10-3118:21:43.653257143\nkeyid=B29D02BB\nexpires={}\n", parse_ttl("1d 1h".to_string()).unwrap()));
    }

    #[test]
    pub fn it_validates_keyhashes() {
        assert_eq!(same_hash_than_in_nodeslist(
        get_pubkey("MIIBCAKCAQEAlntroa72gD50MehPoyp6mRS5fzZpsZEHu42vq9KKxbqSsjfUmxnT
Rsi8CDvBt7DApIc7W1g0eJ6AsOfV7CEh3ooiyL/fC9SGATyDg5TjYPJZn3MPUktg
YBzTd1MMyZL6zcLmIpQBH6XHkH7Do/RxFRtaSyicLxiO3H3wapH20TnkUvEpV5Qh
zUkNM8vHZuu3m1FgLrK5NCN7BtoGWgeyVJvBMbWww5hS15IkCRuBkAOK/+h8xe2f
hMQjrt9gW2qJpxZyFoPuMsWFIaX4wrN7Y8ZiN37U2q1G11tv2oQlJTQeiYaUnTX4
z5VEb9yx2KikbWyChM1Akp82AV5BzqE80QIBIw==".to_string()).unwrap(),
        HashType::Sha512,
        "3c4641f2e99ade126b9923ffdfc432d486043e11ce7bd5528cc50ed372fc4c224dba7f2a2a3a24f114c06e42af5f45f5c248abd7ae300eeefc27bcf0687d7040"
    ).unwrap(), true);
    }

    #[test]
    pub fn it_validates_signatures() {
        // Generate a keypair
        let k0 = Rsa::generate(2048).unwrap();
        let k0pkey = k0.public_key_to_pem().unwrap();
        let k1 = Rsa::public_key_from_pem(&k0pkey).unwrap();

        let keypriv = PKey::from_rsa(k0).unwrap();
        let keypub = PKey::from_rsa(k1).unwrap();

        let data = b"hello, world!";

        // Sign the data
        let mut signer = Signer::new(HashType::Sha512.to_openssl_hash(), &keypriv).unwrap();
        signer.update(data).unwrap();

        let signature = signer.sign_to_vec().unwrap();

        assert!(validate_signature(data, keypub, HashType::Sha512, &signature).unwrap());
    }
}
