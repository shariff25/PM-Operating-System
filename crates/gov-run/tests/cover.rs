//! The Authority program's watch on the witness covering its own chain.

use gov_run::Cover;

#[test]
fn a_head_the_witness_reaches_in_time_is_no_alarm_and_one_it_never_reaches_is() {
    let mut cover = Cover::default();
    // A chain that keeps moving, with the witness one step behind each time.
    for step in 0..100u64 {
        let now = 1_000 + step as i64 * 60;
        assert!(!cover.overdue(step + 1, step, now, 180), "step {step}");
    }
    // The witness stops at 100; the chain goes on.
    assert!(!cover.overdue(101, 100, 7_000, 180));
    assert!(!cover.overdue(105, 100, 7_180, 180));
    assert!(cover.overdue(109, 100, 7_181, 180));
    // It catches up with the head that was waiting: the alarm ends.
    assert!(!cover.overdue(110, 101, 7_200, 180));
    assert!(!cover.overdue(110, 110, 9_000, 180));
}
