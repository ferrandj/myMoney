//! Which periods of an account no imported statement covers, and which of those the user said are fine.
//!
//! A gap is a stretch of days between the statements of one account that none of them covers (statements that
//! overlap leave no gap). Each one is reported until the missing statement is imported, or the user marks it as normal
//! (an account with nothing to report that month, a statement the bank never issued...). A "normal" mark covers the
//! gap it was given for and anything inside it: when a partial statement later splits the gap, the pieces stay quiet,
//! but a new, larger or different hole is reported again.

use chrono::{Datelike, Duration, NaiveDate};
use rusqlite::Connection;

use crate::error::{value_err, AppResult};
use crate::models::{Account, GapAck, Statement};
use crate::py;
use crate::services::{access, statements};

pub const MONTHS: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];

#[derive(Clone, Debug)]
pub struct Gap {
    pub account_id: i64,
    pub account_name: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    /// set when the user marked it as normal
    pub ack_id: Option<i64>,
}

impl Gap {
    pub fn acknowledged(&self) -> bool {
        self.ack_id.is_some()
    }
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }
    pub fn label(&self) -> String {
        describe(self.start, self.end)
    }
}

pub fn last_day(year: i32, month: u32) -> u32 {
    let next = if month == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1) } else { NaiveDate::from_ymd_opt(year, month + 1, 1) };
    next.unwrap().pred_opt().unwrap().day()
}

/// "mai 2026", "mai → juillet 2026", or the exact dates when the gap does not follow whole months.
pub fn describe(start: NaiveDate, end: NaiveDate) -> String {
    let whole = start.day() == 1 && end.day() == last_day(end.year(), end.month());
    if !whole {
        return format!("du {} au {}", start.format("%d/%m/%Y"), end.format("%d/%m/%Y"));
    }
    if (start.year(), start.month()) == (end.year(), end.month()) {
        return format!("{} {}", MONTHS[start.month0() as usize], start.year());
    }
    let first = format!("{}{}", MONTHS[start.month0() as usize], if start.year() == end.year() { String::new() } else { format!(" {}", start.year()) });
    format!("{first} → {} {}", MONTHS[end.month0() as usize], end.year())
}

/// Uncovered stretches between periods, whatever their order or overlaps.
pub fn gaps_between(periods: &[(NaiveDate, NaiveDate)]) -> Vec<(NaiveDate, NaiveDate)> {
    let mut sorted = periods.to_vec();
    sorted.sort();
    let mut gaps = vec![];
    let mut covered_until: Option<NaiveDate> = None;
    for (start, end) in sorted {
        if let Some(cu) = covered_until {
            if start - cu > Duration::days(1) {
                gaps.push((cu + Duration::days(1), start - Duration::days(1)));
            }
        }
        covered_until = Some(match covered_until {
            None => end,
            Some(cu) => cu.max(end),
        });
    }
    gaps
}

pub fn statement_periods(c: &Connection, account_id: i64) -> AppResult<Vec<(NaiveDate, NaiveDate)>> {
    Ok(Statement::query(c, "WHERE account_id = ?", [account_id])?.into_iter().filter_map(|s| Some((s.period_start?, s.period_end?))).collect())
}

pub fn account_gaps(c: &Connection, account: &Account, include_acknowledged: bool) -> AppResult<Vec<Gap>> {
    let periods = statement_periods(c, account.id)?;
    let acks = GapAck::query(c, "WHERE account_id = ?", [account.id])?;
    let waiting = statements::waiting_periods(c, account, None)?; // read but not validated yet: waiting, not missing
    let mut result = vec![];
    for gap in gaps_between(&periods) {
        for (start, end) in statements::uncovered(gap.0, gap.1, &waiting) {
            let ack = acks.iter().find(|a| a.period_start <= start && end <= a.period_end);
            if ack.is_none() || include_acknowledged {
                result.push(Gap { account_id: account.id, account_name: account.name.clone(), start, end, ack_id: ack.map(|a| a.id) });
            }
        }
    }
    Ok(result)
}

/// Do the imported statements of the account cover every day of a period?
pub fn covers(c: &Connection, account_id: i64, start: NaiveDate, end: NaiveDate) -> AppResult<bool> {
    let mut periods = statement_periods(c, account_id)?;
    periods.sort();
    let mut reached = start - Duration::days(1);
    for (ps, pe) in periods {
        if ps > reached + Duration::days(1) {
            break;
        }
        reached = reached.max(pe);
    }
    Ok(reached >= end)
}

/// Gaps of every account that is still followed (archived accounts are left alone): the ones the user can fix, that
/// is the accounts they can edit (someone who only reads an account has no statement to upload for it).
pub fn all_gaps(c: &Connection, include_acknowledged: bool) -> AppResult<Vec<Gap>> {
    let accounts = access::only_editable(c, Account::query(c, "WHERE archived = 0 ORDER BY institution, name", [])?)?;
    let mut out = vec![];
    for a in &accounts {
        out.extend(account_gaps(c, a, include_acknowledged)?);
    }
    Ok(out)
}

pub fn acknowledged_ranges(c: &Connection, account_id: i64) -> AppResult<Vec<(NaiveDate, NaiveDate)>> {
    Ok(GapAck::query(c, "WHERE account_id = ?", [account_id])?.into_iter().map(|a| (a.period_start, a.period_end)).collect())
}

/// Mark a gap as normal. Only a gap that exists can be marked: a stale form changes nothing.
pub fn acknowledge(c: &Connection, account_id: i64, start: NaiveDate, end: NaiveDate, note: Option<&str>) -> AppResult<()> {
    access::check_edit(c, account_id)?;
    let account = Account::get(c, account_id)?;
    let Some(account) = account.filter(|_| end >= start) else { return value_err("Compte ou période invalide") };
    if !account_gaps(c, &account, false)?.iter().any(|g| g.start <= start && end <= g.end) {
        return value_err("Cette période n'est plus un trou : recharge la page");
    }
    let note = py::strip(note.unwrap_or("")).to_string();
    GapAck { account_id, period_start: start, period_end: end, note: if note.is_empty() { None } else { Some(note) }, ..Default::default() }.insert(c)?;
    Ok(())
}

/// Report the gap again (the user changed their mind).
pub fn restore(c: &Connection, ack_id: i64) -> AppResult<()> {
    if let Some(ack) = GapAck::get(c, ack_id)? {
        access::check_edit(c, ack.account_id)?;
        GapAck::delete(c, ack.id)?;
    }
    Ok(())
}
