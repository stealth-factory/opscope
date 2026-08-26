
// A command that never answers must be given up on, and killed.
#[test]
fn a_command_that_never_answers_is_given_up_on() {
    let began = std::time::Instant::now();
    let got = toys_core::run(&["sleep", "30"], 2);
    let took = began.elapsed().as_secs_f64();
    assert!(got.is_err(), "sleep 30 returned {:?}", got);
    assert!(took < 5.0, "waited {:.1}s for a 2s limit", took);
    assert!(
        got.unwrap_err().contains("did not answer"),
        "the reason should say what happened"
    );
}

#[test]
fn a_command_that_answers_comes_back_with_its_output() {
    let got = toys_core::run(&["echo", "hello"], 5).expect("echo");
    assert_eq!(got.trim(), "hello");
}

#[test]
fn a_command_that_fails_is_an_error_not_empty_output() {
    // `false` prints nothing and exits 1. The old run() turned that into
    // an empty string, which reads on screen as a source with no data.
    assert!(toys_core::run(&["false"], 5).is_err());
}

#[test]
fn a_command_that_is_not_installed_says_so() {
    let got = toys_core::run(&["definitely-not-a-real-binary-xyz"], 5);
    assert!(got.is_err());
}
