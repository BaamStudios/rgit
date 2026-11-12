// SPDX-License-Identifier: MIT
//! Parser tests. Expected values were produced by git 2.56.0
//! (`git rev-parse --since=<s>` with `GIT_TEST_DATE_NOW=1600000000`,
//! `git var GIT_COMMITTER_IDENT` with `GIT_COMMITTER_DATE=<s>`).

use rgit_date::{
    DateContext, DateError, TimeZone, approxidate, approxidate_careful, format_raw, parse_date,
    parse_expiry_date,
};

const NOW: i64 = 1600000000; // Sun 2020-09-13 12:26:40 UTC

fn utc() -> DateContext {
    DateContext::utc(NOW)
}

fn berlin() -> DateContext {
    DateContext::new(NOW, TimeZone::get("Europe/Berlin").unwrap())
}

#[test]
fn approxidate_utc() {
    for (input, want) in [
        ("now", 1600000000),
        ("today", 1599955200),
        ("yesterday", 1599913600),
        ("noon", 1599998400),
        ("midnight", 1599955200),
        ("tea", 1599930000),
        ("never", 0),
        ("3 hours ago", 1599989200),
        ("4.days.ago", 1599654400),
        ("2 weeks", 1598790400),
        ("3 months ago", 1592051200),
        ("1 year 2 months ago", 1563020800),
        ("last monday", 1599481600),
        ("2 friday", 1599222400),
        ("last wednesday at noon", 1599652800),
        ("9am", 1599987600),
        ("9 pm yesterday", 1599944400),
        ("17:45", 1600019100),
        ("Mar 3", 1583238400),
        ("3 March 2015", 1425385600),
        ("March 3rd, 2015", 1425385600),
        ("2015-03-03", 1425385600),
        ("03/04/2015", 1425472000),
        ("04.03.2015", 1425472000),
        ("3/4", 1583324800),
        ("4.3", 1583324800),
        ("Dec 24", 1577190400),
        ("6 months ago Jan", 1578918400),
        ("1500000000", 1500000000),
        ("@1500000000 +0200", 1500000000),
        ("2015-03-03 10:11:12 +0100", 1425373872),
        ("garbage", 1600000000),
    ] {
        assert_eq!(approxidate(input, &utc()), want, "{input:?}");
    }
}

#[test]
fn approxidate_local_zone() {
    // Local wall clock times are interpreted in the local zone.
    assert_eq!(approxidate("17:45", &berlin()), 1600011900);
    // The time of day comes from now (14:26:40 CEST) and keeps being read as
    // summer time although March 3 is in winter, like git does.
    assert_eq!(approxidate("2015-03-03", &berlin()), 1425385600);
    assert_eq!(approxidate("yesterday", &berlin()), 1599913600);
}

#[test]
fn approxidate_errors() {
    assert_eq!(approxidate_careful("garbage", &utc()), (1600000000, true));
    assert_eq!(approxidate_careful("friday", &utc()), (1600000000, true));
    assert_eq!(approxidate_careful("weeks", &utc()), (1600000000, true));
    assert!(!approxidate_careful("last friday", &utc()).1);
    assert!(!approxidate_careful("5", &utc()).1);
}

#[test]
fn expiry() {
    assert_eq!(parse_expiry_date("never", &utc()), Ok(0));
    assert_eq!(parse_expiry_date("now", &utc()), Ok(u64::MAX));
    assert_eq!(parse_expiry_date("all", &utc()), Ok(u64::MAX));
    assert_eq!(parse_expiry_date("2 weeks ago", &utc()), Ok(1598790400));
    assert_eq!(
        parse_expiry_date("garbage", &utc()),
        Err(DateError::InvalidTimestamp("garbage".into()))
    );
}

#[test]
fn strict() {
    for (input, want_utc, want_berlin) in [
        (
            "2015-03-03 10:11:12",
            Some("1425377472 +0000"),
            Some("1425373872 +0100"),
        ),
        (
            "2015-03-03 10:11:12 -0330",
            Some("1425390072 -0330"),
            Some("1425390072 -0330"),
        ),
        (
            "2015-03-03T10:11:12+05:45",
            Some("1425356772 +0545"),
            Some("1425356772 +0545"),
        ),
        (
            "Tue, 3 Mar 2015 10:11:12 +0100",
            Some("1425373872 +0100"),
            Some("1425373872 +0100"),
        ),
        (
            "Tue Mar 3 10:11:12 2015 -0800",
            Some("1425406272 -0800"),
            Some("1425406272 -0800"),
        ),
        (
            "20150303T101112",
            Some("1425377472 +0000"),
            Some("1425373872 +0100"),
        ),
        (
            "20150303T1011",
            Some("1425377460 +0000"),
            Some("1425373860 +0100"),
        ),
        (
            "03/04/2015 10:11",
            Some("1425463860 +0000"),
            Some("1425460260 +0100"),
        ),
        (
            "04.03.2015 10:11",
            Some("1425463860 +0000"),
            Some("1425460260 +0100"),
        ),
        (
            "2015-03-03 10:11:12 PST",
            Some("1425406272 -0800"),
            Some("1425406272 -0800"),
        ),
        (
            "1500000000 +0200",
            Some("1500000000 +0200"),
            Some("1500000000 +0200"),
        ),
        (
            "1500000000",
            Some("1500000000 +0000"),
            Some("1500000000 +0200"),
        ),
        (
            "1500000000 10:00",
            Some("1500026400 +0000"),
            Some("1500026400 +0200"),
        ),
        (
            "@1500000000 -0115",
            Some("1500000000 -0115"),
            Some("1500000000 -0115"),
        ),
        ("@5 +0000", Some("5 +0000"), Some("5 +0000")),
        ("@5", None, None),
        ("2015-03-03", None, None),
        ("10:11:12", None, None),
        ("1969-12-31 23:00:00", None, None),
        ("2100-01-01 00:00:00", None, None),
        (
            "2015-03-03 10:11:12.75",
            Some("1425377472 +0000"),
            Some("1425373872 +0100"),
        ),
        ("yesterday", None, None),
    ] {
        let got = parse_date(input, &utc()).map(|(t, tz)| format_raw(t, tz));
        assert_eq!(got.as_deref(), want_utc, "{input:?} (UTC)");
        let got = parse_date(input, &berlin()).map(|(t, tz)| format_raw(t, tz));
        assert_eq!(got.as_deref(), want_berlin, "{input:?} (Europe/Berlin)");
    }
}
