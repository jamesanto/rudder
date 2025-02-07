/*
*************************************************************************************
* Copyright 2013 Normation SAS
*************************************************************************************
*
* This file is part of Rudder.
*
* Rudder is free software: you can redistribute it and/or modify
* it under the terms of the GNU General Public License as published by
* the Free Software Foundation, either version 3 of the License, or
* (at your option) any later version.
*
* In accordance with the terms of section 7 (7. Additional Terms.) of
* the GNU General Public License version 3, the copyright holders add
* the following Additional permissions:
* Notwithstanding to the terms of section 5 (5. Conveying Modified Source
* Versions) and 6 (6. Conveying Non-Source Forms.) of the GNU General
* Public License version 3, when you create a Related Module, this
* Related Module is not considered as a part of the work and may be
* distributed under the license agreement of your choice.
* A "Related Module" means a set of sources files including their
* documentation that, without modification of the Source Code, enables
* supplementary functions or services in addition to those offered by
* the Software.
*
* Rudder is distributed in the hope that it will be useful,
* but WITHOUT ANY WARRANTY; without even the implied warranty of
* MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
* GNU General Public License for more details.
*
* You should have received a copy of the GNU General Public License
* along with Rudder.  If not, see <http://www.gnu.org/licenses/>.

*
*************************************************************************************
*/

package com.normation.rudder.reports.execution


import java.util.concurrent.ScheduledThreadPoolExecutor

import scala.concurrent.Future
import scala.concurrent.ExecutionContext.Implicits.global
import com.normation.inventory.domain.NodeId
import com.normation.rudder.batch.FindNewReportsExecution
import com.normation.rudder.domain.logger.ReportLogger
import com.normation.rudder.repository.ReportsRepository
import com.normation.rudder.services.reports.CachedFindRuleNodeStatusReports
import com.normation.rudder.services.reports.CachedNodeChangesServiceImpl
import com.normation.utils.Control
import org.joda.time.DateTime
import org.joda.time.format.PeriodFormat
import net.liftweb.common._
import com.normation.rudder.db.DB
import com.normation.rudder.repository.ComplianceRepository
import monix.execution.schedulers.ExecutorScheduler

import scala.concurrent.duration.FiniteDuration

/**
 * That service contains most of the logic to merge
 * interval of same criticity together.
 */
class ReportsExecutionService (
    reportsRepository      : ReportsRepository
  , writeExecutions        : WoReportsExecutionRepository
  , statusUpdateRepository : LastProcessedReportRepository
  , cachedChanges          : CachedNodeChangesServiceImpl
  , cachedCompliance       : CachedFindRuleNodeStatusReports
  , complianceRepos        : ComplianceRepository
  , catchupFromDuration    : FiniteDuration
  , catchupInterval        : FiniteDuration
  , computeChangeEnabled   : () => Box[Boolean]
) {

  //use that threadpool for changes. It's single threaded because everything is blocking
  //below, so it's better to add task in a queue that thread everywhere
  val changesThreadPool = monix.execution.Scheduler.singleThread("rudder-changes-hook").asInstanceOf[ExecutorScheduler]

  val logger = ReportLogger
  var idForCheck: Long = 0

  private[this] def computeCatchupEndTime(catchupFromDateTime: DateTime) : DateTime= {
    // Get reports of the last id and before last report date plus the maxCatchup interval
    if (catchupFromDateTime.plus(catchupInterval.toMillis).isAfter(DateTime.now)) {
      DateTime.now
    } else {
      catchupFromDateTime.plus(catchupInterval.toMillis)
    }
  }

  private[this] def computeFromToReport(lastReportId: Long, lastReportDate: DateTime) : (DateTime, Long, DateTime) = {
    val (catchupFromDateTime: DateTime, fromReportId: Long) =
      (if (lastReportDate.isBefore(DateTime.now.minus(catchupFromDuration.toMillis))) {
        // we are trying to catchup on too long, truncating
        val fromDate = DateTime.now.minus(catchupFromDuration.toMillis)

        reportsRepository.getMaxIdBeforeDateTime(lastReportId, fromDate) match {
          case Full(id) => (fromDate, id)
          case eb:EmptyBox => logger.error(s"Could not correctly retrieve from were to retrieve report processing, fallbacking to default value, error is is ${eb}")
            (fromDate, lastReportId)
        }
      } else {
        (lastReportDate, lastReportId)
      })
    (catchupFromDateTime, fromReportId, computeCatchupEndTime(catchupFromDateTime))
  }

  def findAndSaveExecutions(processId : Long): Box[Option[DB.StatusUpdate]] = {

    val startTime = DateTime.now().getMillis()
    // Get execution status

    statusUpdateRepository.getExecutionStatus match {
      // Find it, start looking for new executions
      case Full(Some((lastReportId,lastReportDate))) =>
        //a test to let the user that there is some inconsistencies in the run
        //we are tracking, and that it may have serious problem.
        if(idForCheck != 0 && lastReportId != idForCheck) {
          logger.error(s"There is an inconsistency in the processed agent runs: last process report id should be ${idForCheck} " +
              s"but the value ${lastReportId} was retrieve from base. Check that you don't have several Rudder application " +
              s"using the same database, or report that message to you support")
        }

        // compute the interval we'll catch up report on
        // we cannot catchup more than catchupInterval at once, and we start from catchupFromDuration
        val (catchupFromDateTime, fromReportId, endBatchDate) = computeFromToReport(lastReportId, lastReportDate)

        logger.debug(s"[${FindNewReportsExecution.SERVICE_NAME} #${processId}] checking agent runs from SQL ID ${fromReportId} " +
            s"[${lastReportDate.toString()} - ${endBatchDate.toString()}]")

        reportsRepository.getReportsfromId(fromReportId, endBatchDate) match {
          case Full((reportExec, maxReportId)) =>
            if (reportExec.size > 0) {

              val maxDate = {
                // Keep the last report date if the last processed report is after all reports processed in this batch
                val maxReportsDate = reportExec.maxBy(_.agentRunId.date.getMillis()).agentRunId.date
                if (maxReportsDate isAfter lastReportDate) {
                  maxReportsDate
                } else {
                  lastReportDate
                }
              }


              // Save new executions
              val res = Control.boxSequence(writeExecutions.updateExecutions(reportExec)) match {
                case Full(result) =>
                  val executionTime = DateTime.now().getMillis() - startTime
                  logger.debug(s"[${FindNewReportsExecution.SERVICE_NAME} #${processId}] (${executionTime} ms) " +
                      s"Added or updated ${result.size} agent runs, up to SQL ID ${maxReportId} (last run time was ${maxDate.toString()})")
                  idForCheck = maxReportId
                  statusUpdateRepository.setExecutionStatus(maxReportId, maxDate)

                case eb:EmptyBox => val fail = eb ?~! "could not save nodes executions"
                  logger.error(s"Could not save execution of Nodes from report ID ${fromReportId} - date ${catchupFromDateTime} to " +
                      s"${endBatchDate.toString()}, cause is : ${fail.messageChain}")
                  eb
              }

              /*
               * here is a hook to plug the update for changes.
               * it should be made generic and inverted with some hooks method defined in
               * that class (giving to caller the lowed and highest index of reports to
               * take into account) BUT: these users will be extremely hard on the DB
               * performance, most likelly each of them doing a query. Of course, it is
               * much more interesting to mutualise the queries (and if possible, make
               * them asynchrone from the main current loop), what is much harder with
               * an IoC pattern.
               * So, until we have a better view of what the actual user are, I let that
               * here.
               */
              val completedRuns = reportExec.filter( _.isCompleted )
              this.hook(lastReportId, maxReportId, completedRuns.map { _.agentRunId.nodeId}.toSet )
              // end of hooks code

              res.map( Some(_) )

            } else {
              val executionTime = DateTime.now().getMillis() - startTime
              logger.debug(s"[${FindNewReportsExecution.SERVICE_NAME} #${processId}] (${executionTime} ms) " +
                  s"Added or updated 0 agent runs")
              idForCheck = lastReportId
              statusUpdateRepository.setExecutionStatus(lastReportId, endBatchDate).map( Some(_) )
            }

          case eb:EmptyBox =>
            val fail = eb ?~! "could not get Reports"
            logger.error(s"Could not get node execution reports in the RudderSysEvents table, cause is: ${fail.messageChain}")
            eb
        }

      // Executions status not initialized ... initialize it!
      case Full(None) =>
        reportsRepository.getReportsWithLowestId match {
          case Full(Some((id, report))) =>
            logger.debug(s"Initializing the status execution update to  id ${id}, date ${report.executionTimestamp}")
            idForCheck = id
            statusUpdateRepository.setExecutionStatus(id, report.executionTimestamp).map( Some(_) )
          case Full(None) =>
            logger.debug("There are no node execution in the database, cannot save the execution")
            Full( None )
          case eb:EmptyBox =>
            val fail = eb ?~! "Could not get Reports with lowest id from the RudderSysEvents table"
            logger.error(s"Could not get reports from the database, cause is: ${fail.messageChain}")
            fail
        }
      case eb:EmptyBox =>
        val fail = eb ?~! "Could not get node execution status"
        logger.error(s"Could not get node executions reports from the RudderSysEvents table  cause is: ${fail.messageChain}")
        fail

    }
  }

  /*
   * The hook method where the other method needing to happen when
   * new reports are processed are called.
   */
  private[this] def hook(lowestId: Long, highestId: Long, updatedNodeIds: Set[NodeId]) : Unit = {
    val startHooks = System.currentTimeMillis

    if(computeChangeEnabled().getOrElse(true)) {
    logger.debug(s"New task in 'update changes cache' queue; Waiting tasks: ${changesThreadPool.executor.asInstanceOf[ScheduledThreadPoolExecutor].getQueue.size()}")
    Future {
      //update changes by rules
      (for {
        changes <- reportsRepository.getChangeReportsOnInterval(lowestId, highestId)
        updated <- cachedChanges.update(changes)
      } yield {
        updated
      }) match {
        case eb: EmptyBox =>
          val e = eb ?~! "An error occured when trying to update the cache of last changes"
          logger.error(e.messageChain)
          e.rootExceptionCause.foreach { ex =>
            logger.error("Root exception was: ", ex)
          }
        case Full(x) =>
          logger.trace("Cache for changes by rule updates after new run received")
      }
    }(changesThreadPool) // exec on their own threadpool the change computation
    } else {
      logger.warn(s"Not updating changes by rule - disabled by settings 'rudder_compute_changes'")
    }

    Future {
      // update compliance cache
      cachedCompliance.invalidate(updatedNodeIds) match {
        case eb: EmptyBox =>
          val e = eb ?~! "An error occurred when trying to update the cache for compliance"
          logger.error(e.messageChain)
          e.rootExceptionCause.foreach { ex =>
            logger.error("Root exception was: ", ex)
          }
        case Full(x) =>
          //save compliance in DB
          complianceRepos.saveRunCompliance(x.values.toList)
          logger.trace("Cache for compliance updates after new run received")
      }
    }
    import org.joda.time.Duration
    logger.debug(s"Hooks execution time: ${PeriodFormat.getDefault().print(Duration.millis(System.currentTimeMillis - startHooks).toPeriod())}")

  }

}
