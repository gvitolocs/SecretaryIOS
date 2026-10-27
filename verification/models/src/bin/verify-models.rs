//! Runs every model case and prints a summary table; exit 1 on any
//! mismatch with the expectations. `--deep` lifts the depth bounds,
//! `--only <substring>` filters cases, `--traces` prints counterexamples.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let deep = args.iter().any(|a| a == "--deep") || cfg!(feature = "deep");
    let traces = args.iter().any(|a| a == "--traces");
    let states = args.iter().any(|a| a == "--states");
    let only = args
        .iter()
        .position(|a| a == "--only")
        .and_then(|i| args.get(i + 1).cloned());
    let mut failed = 0;
    println!(
        "{:<12} {:<52} {:>10} {:>6} {:>8}  result",
        "protocol", "case", "states", "depth", "secs"
    );
    for case in verification_models::all_cases(deep) {
        if only
            .as_deref()
            .is_some_and(|o| !format!("{} {}", case.protocol, case.name).contains(o))
        {
            continue;
        }
        let (r, res) = case.verify();
        let verdicts: Vec<String> = case
            .expect
            .iter()
            .map(|(n, e)| {
                let p = r.prop(n);
                let mark = match (e, p.discovered) {
                    (verification_models::Expect::Holds, false) => "ok",
                    (verification_models::Expect::Violated, true) => "BUG-WITNESS",
                    (verification_models::Expect::Reached, true) => "reached",
                    _ => "MISMATCH",
                };
                format!("{n}={mark}")
            })
            .collect();
        println!(
            "{:<12} {:<52} {:>10} {:>6} {:>8.1}  {}{}",
            case.protocol,
            case.name,
            r.unique_states,
            r.max_depth,
            r.elapsed.as_secs_f64(),
            if res.is_ok() { "PASS " } else { "FAIL " },
            verdicts.join(" ")
        );
        if traces {
            for (n, e) in &case.expect {
                if *e == verification_models::Expect::Violated {
                    if let Some(t) = &r.prop(n).trace {
                        println!("    counterexample for {n}:\n{t}\n");
                    }
                }
            }
        }
        if let Err(e) = res {
            failed += 1;
            println!("    {}", e.replace('\n', "\n    "));
            if states {
                for (n, _) in &case.expect {
                    if let Some(st) = &r.prop(n).last_state {
                        println!("    last state for {n}:\n{st}");
                    }
                }
            }
        }
    }
    if failed == 0 {
        println!("MODELS GREEN");
        ExitCode::SUCCESS
    } else {
        println!("MODELS FAILED: {failed} case(s)");
        ExitCode::FAILURE
    }
}
