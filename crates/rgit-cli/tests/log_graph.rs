// SPDX-License-Identifier: MIT
//! `rgit log --graph` against outputs recorded from the reference git
//! (v2.56; older versions draw roots without indentation, so these are not
//! compared live).

use rgit_difftest::Fixture;

/// A history with a three-parent merge, crossing lanes and three roots.
fn history() -> Option<Fixture> {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"]);
    if fx.is_skipped() {
        return None;
    }
    let tree = String::from_utf8(fx.git_stdout(["hash-object", "-t", "tree", "-w", "/dev/null"]))
        .unwrap()
        .trim()
        .to_string();
    let mut fx = fx;
    let mut ids: Vec<(&str, String)> = Vec::new();
    let spec: [(&str, &[&str]); 14] = [
        ("a", &[]),
        ("b", &["a"]),
        ("c", &["a"]),
        ("d", &["a"]),
        ("oct", &["b", "c", "d"]),
        ("e", &["oct"]),
        ("f", &["a"]),
        ("g", &["f"]),
        ("m", &["e", "g"]),
        ("r1", &[]),
        ("r2", &[]),
        ("r3", &[]),
        ("s", &["c"]),
        ("top", &["s", "m"]),
    ];
    for (name, parents) in spec {
        let mut args = vec![
            "commit-tree".to_string(),
            tree.clone(),
            "-m".into(),
            name.into(),
        ];
        for p in parents {
            let id = &ids.iter().find(|(n, _)| n == p).unwrap().1;
            args.push("-p".into());
            args.push(id.clone());
        }
        let id = String::from_utf8(fx.git_stdout(&args))
            .unwrap()
            .trim()
            .to_string();
        fx = fx
            .git(["update-ref", &format!("refs/heads/{name}"), &id])
            .tick();
        ids.push((name, id));
    }
    Some(fx)
}

fn rgit(fx: &mut Fixture, args: &[&str]) -> String {
    let outcome = fx.compare(args).checks([]).run().expect("not skipped");
    assert_eq!(outcome.rgit.code, Some(0), "{args:?}");
    let out = String::from_utf8(outcome.rgit.stdout).unwrap();
    // Lines are padded to the width of the graph; the expectations below
    // leave out the trailing blanks (checked separately).
    out.split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn graph_pads_lines() {
    let Some(mut fx) = history() else { return };
    let outcome = fx
        .compare(["log", "--graph", "--format=%s", "-2", "m"])
        .checks([])
        .run()
        .expect("not skipped");
    assert_eq!(outcome.rgit.stdout, b"*   m\n|\\  \n| * g\n");
}

#[test]
fn graph_all() {
    let Some(mut fx) = history() else { return };
    let want = "\
*   top
|\\
| *   m
| |\\
| | * g
| | * f
| * | e
| | |
| |  \\
| *-. \\   oct
| |\\ \\ \\
| | | * | d
| | | |/
| * | / b
| | |/
| |/|
* | | s
| |/
|/|
* | c
|/
* a
  * r3
    * r2
* r1
";
    assert_eq!(
        rgit(&mut fx, &["log", "--graph", "--all", "--format=%s"]),
        want
    );
    // Without indentation the roots line up.
    let flat = want.replace("  * r3\n    * r2\n", "* r3\n* r2\n");
    assert_eq!(
        rgit(
            &mut fx,
            &[
                "log",
                "--graph",
                "--all",
                "--format=%s",
                "--no-graph-indent"
            ]
        ),
        flat
    );
}

#[test]
fn graph_colors() {
    let Some(mut fx) = history() else { return };
    let r = "\x1b[31m";
    let g = "\x1b[32m";
    let b = "\x1b[34m";
    let e = "\x1b[m";
    let want = format!(
        "*   top
{r}|{e}{b}\\{e}
{r}|{e} *   m
{r}|{e} {g}|{e}{r}\\{e}
{r}|{e} {g}|{e} * g
{r}|{e} {g}|{e} * f
{r}|{e} * {r}|{e} e
{r}|{e} {r}|{e} {r}|{e}
{r}|{e} {r}|{e}  {r}\\{e}
{r}|{e} *{r}-{e}{r}.{e} {r}\\{e}   oct
{r}|{e} {b}|{e}{g}\\{e} {r}\\{e} {r}\\{e}
{r}|{e} {b}|{e} {g}|{e} * {r}|{e} d
{r}|{e} {b}|{e} {g}|{e} {r}|{e}{r}/{e}
{r}|{e} * {g}|{e} {r}/{e} b
{r}|{e} {r}|{e} {g}|{e}{r}/{e}
{r}|{e} {r}|{e}{r}/{e}{g}|{e}
* {r}|{e} {g}|{e} s
{g}|{e} {r}|{e}{g}/{e}
{g}|{e}{g}/{e}{r}|{e}
* {r}|{e} c
{r}|{e}{r}/{e}
* a
"
    );
    let got = rgit(
        &mut fx,
        &[
            "-c",
            "log.graphColors=red,blue,green",
            "log",
            "--graph",
            "--color=always",
            "--format=%s",
            "top",
        ],
    );
    assert_eq!(got, want);
}

#[test]
fn graph_separators_boundary_and_limits() {
    let Some(mut fx) = history() else { return };
    let want = "\
*   m
|\\
| |
| * g
| |
| * f
| |
* | e
| |
|  \\
*-. \\   oct
|\\ \\ \\
| | | |
| | * | d
| | |/
| | |
| * | c
| |/
| |
* | b
|/
|
* a
";
    assert_eq!(
        rgit(&mut fx, &["log", "--graph", "--format=format:%s%n%b", "m"]),
        want
    );
    let want = "\
*   top
|\\
| *   m
| |\\
| | * g
| | * f
| * | e
* | | s
| o | oct
|/ /
o / c
|/
o a
";
    assert_eq!(
        rgit(
            &mut fx,
            &["log", "--graph", "--boundary", "--format=%s", "oct..top"]
        ),
        want
    );
    assert_eq!(
        rgit(
            &mut fx,
            &["log", "--graph", "--first-parent", "--format=%s", "top"]
        ),
        "* top\n* s\n* c\n* a\n"
    );
    assert_eq!(
        rgit(
            &mut fx,
            &[
                "log",
                "--graph",
                "--format=%s",
                "--max-count-oldest=3",
                "top"
            ]
        ),
        "* s\n* c\n* a\n"
    );
}

#[test]
fn graph_conflicts() {
    let Some(mut fx) = history() else { return };
    let out = fx
        .compare(["log", "--graph", "--reverse"])
        .checks([])
        .run()
        .expect("not skipped");
    assert_eq!(out.rgit.code, Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.rgit.stderr),
        "fatal: options '--graph' and '--reverse' cannot be used together\n"
    );
}
