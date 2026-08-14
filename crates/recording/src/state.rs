//! Recording state machine. Pure logic, no I/O.

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Recording,
    Paused,
    Stopped,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Idle => "Idle",
            State::Recording => "Recording",
            State::Paused => "Paused",
            State::Stopped => "Stopped",
        }
    }
}

pub struct StateMachine {
    state: State,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    pub fn new() -> Self {
        Self { state: State::Idle }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn start(&mut self) -> Result<()> {
        self.transition(State::Recording, |s| matches!(s, State::Idle))
    }

    pub fn pause(&mut self) -> Result<()> {
        self.transition(State::Paused, |s| matches!(s, State::Recording))
    }

    pub fn resume(&mut self) -> Result<()> {
        self.transition(State::Recording, |s| matches!(s, State::Paused))
    }

    pub fn stop(&mut self) -> Result<()> {
        self.transition(State::Stopped, |s| {
            matches!(s, State::Recording | State::Paused)
        })
    }

    fn transition<F: Fn(State) -> bool>(&mut self, to: State, allowed: F) -> Result<()> {
        if allowed(self.state) {
            self.state = to;
            Ok(())
        } else {
            Err(crate::error::RecordingError::InvalidTransition {
                from: self.state.name(),
                to: to.name(),
            })
        }
    }
}
