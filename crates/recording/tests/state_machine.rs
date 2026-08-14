use recording::state::{State, StateMachine};
use recording::RecordingError;

#[test]
fn happy_path_idle_record_pause_resume_stop() {
    let mut sm = StateMachine::new();
    assert_eq!(sm.state(), State::Idle);

    sm.start().unwrap();
    assert_eq!(sm.state(), State::Recording);

    sm.pause().unwrap();
    assert_eq!(sm.state(), State::Paused);

    sm.resume().unwrap();
    assert_eq!(sm.state(), State::Recording);

    sm.stop().unwrap();
    assert_eq!(sm.state(), State::Stopped);
}

#[test]
fn pause_then_stop_is_valid() {
    let mut sm = StateMachine::new();
    sm.start().unwrap();
    sm.pause().unwrap();
    sm.stop().unwrap();
    assert_eq!(sm.state(), State::Stopped);
}

#[test]
fn cannot_pause_from_idle() {
    let mut sm = StateMachine::new();
    let err = sm.pause().unwrap_err();
    assert!(matches!(err, RecordingError::InvalidTransition { from: "Idle", to: "Paused" }));
    assert_eq!(sm.state(), State::Idle);
}

#[test]
fn cannot_start_twice() {
    let mut sm = StateMachine::new();
    sm.start().unwrap();
    let err = sm.start().unwrap_err();
    assert!(matches!(err, RecordingError::InvalidTransition { .. }));
}

#[test]
fn cannot_resume_from_recording() {
    let mut sm = StateMachine::new();
    sm.start().unwrap();
    let err = sm.resume().unwrap_err();
    assert!(matches!(err, RecordingError::InvalidTransition { .. }));
}

#[test]
fn stopped_is_terminal() {
    let mut sm = StateMachine::new();
    sm.start().unwrap();
    sm.stop().unwrap();
    assert!(sm.start().is_err());
    assert!(sm.pause().is_err());
    assert!(sm.resume().is_err());
    assert!(sm.stop().is_err());
}
