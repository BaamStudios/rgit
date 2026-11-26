// SPDX-License-Identifier: MIT
//! Task selection of `git maintenance run`, as the reference git v2.56
//! makes it (observed with `GIT_TRACE2_EVENT`).

use super::*;

fn config(text: &str) -> ConfigSet {
    rgit_config::parse_bytes(
        text.as_bytes(),
        rgit_config::Origin::CommandLine,
        rgit_config::Scope::Command,
    )
    .unwrap()
}

fn names(text: &str, explicit: &[Task], schedule: Option<Schedule>) -> Vec<&'static str> {
    select(&config(text), explicit, schedule)
        .unwrap()
        .into_iter()
        .map(Task::name)
        .collect()
}

const GEOMETRIC: [&str; 6] = [
    "geometric-repack",
    "commit-graph",
    "pack-refs",
    "reflog-expire",
    "worktree-prune",
    "rerere-gc",
];

#[test]
fn strategies() {
    use Schedule::*;
    assert_eq!(names("", &[], None), GEOMETRIC);
    assert!(names("", &[], Some(Weekly)).is_empty());
    let s = "[maintenance]strategy=geometric";
    assert_eq!(names(s, &[], None), GEOMETRIC);
    assert_eq!(names(s, &[], Some(Hourly)), ["commit-graph"]);
    assert_eq!(
        names(s, &[], Some(Daily)),
        ["geometric-repack", "commit-graph", "pack-refs"]
    );
    assert_eq!(names(s, &[], Some(Weekly)), GEOMETRIC);
    let s = "[maintenance]strategy=gc";
    assert_eq!(names(s, &[], None), ["gc"]);
    assert!(names(s, &[], Some(Hourly)).is_empty());
    assert_eq!(names(s, &[], Some(Daily)), ["gc"]);
    let s = "[maintenance]strategy=incremental";
    assert_eq!(names(s, &[], None), ["gc"]);
    assert_eq!(names(s, &[], Some(Hourly)), ["prefetch", "commit-graph"]);
    assert_eq!(
        names(s, &[], Some(Daily)),
        [
            "prefetch",
            "loose-objects",
            "incremental-repack",
            "commit-graph"
        ]
    );
    assert_eq!(
        names(s, &[], Some(Weekly)),
        [
            "prefetch",
            "loose-objects",
            "incremental-repack",
            "commit-graph",
            "pack-refs"
        ]
    );
}

#[test]
fn task_configuration() {
    use Schedule::*;
    assert_eq!(
        names("[maintenance \"prefetch\"]enabled=true", &[], None)[..3],
        ["prefetch", "geometric-repack", "commit-graph"]
    );
    // Enabled without a frequency, or a frequency without being enabled.
    assert!(names("[maintenance \"prefetch\"]enabled=true", &[], Some(Weekly)).is_empty());
    assert!(
        names(
            "[maintenance \"prefetch\"]schedule=hourly",
            &[],
            Some(Weekly)
        )
        .is_empty()
    );
    let s = "[maintenance \"gc\"]enabled=true\nschedule=hourly";
    assert_eq!(names(s, &[], Some(Hourly)), ["gc"]);
    let s = "[maintenance \"gc\"]enabled=true\nschedule=bogus";
    assert!(names(s, &[], Some(Weekly)).is_empty());
    let s = "[maintenance]strategy=incremental\n\
             [maintenance \"commit-graph\"]schedule=daily\n\
             [maintenance \"loose-objects\"]schedule=hourly\n\
             [maintenance \"incremental-repack\"]enabled=false";
    assert_eq!(names(s, &[], Some(Hourly)), ["prefetch", "loose-objects"]);
    assert_eq!(
        names(s, &[], Some(Daily)),
        ["prefetch", "loose-objects", "commit-graph"]
    );
    // --task wins, in its order.
    assert_eq!(
        names(
            "[maintenance]strategy=gc",
            &[Task::RerereGc, Task::Gc],
            None
        ),
        ["rerere-gc", "gc"]
    );
}

#[test]
fn unknown_strategy() {
    assert!(select(&config("[maintenance]strategy=none"), &[Task::Gc], None).is_err());
}
