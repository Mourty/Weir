//! A stand-in for PipeWire's buffers that drives the [`Processor`] the way
//! the process callback does: for the tests, and, with the `fuzzing`
//! feature, for the fuzz target that plays made-up mixers.

use super::params::{RtInsert, RtParams};
use super::process::Processor;
use std::sync::Arc;
use weir_protocol::MixerState;

/// Test harness: owns buffers and drives the processor.
pub struct Rig {
    pub inputs: Vec<Vec<Vec<f32>>>,  // [strip][ch][k]
    pub outputs: Vec<Vec<Vec<f32>>>, // [bus][ch][k]
    /// What each strip's and bus's external effects are sent, and what
    /// they give back, like `inputs` and `outputs`.
    pub strip_sends: Vec<Vec<Vec<f32>>>,
    pub strip_returns: Vec<Vec<Vec<f32>>>,
    pub bus_sends: Vec<Vec<Vec<f32>>>,
    pub bus_returns: Vec<Vec<Vec<f32>>>,
    pub n: usize,
    pub proc_: Processor,
}

impl Rig {
    pub fn new(state: &MixerState, n: usize) -> Self {
        let strips = || {
            state
                .strips
                .iter()
                .map(|s| vec![vec![0.0; n]; s.layout.channel_count()])
                .collect()
        };
        let buses = || {
            state
                .buses
                .iter()
                .map(|b| vec![vec![0.0; n]; b.layout.channel_count()])
                .collect()
        };
        Self {
            inputs: strips(),
            outputs: buses(),
            strip_sends: strips(),
            strip_returns: strips(),
            bus_sends: buses(),
            bus_returns: buses(),
            n,
            proc_: Processor::new(),
        }
    }

    pub fn run(&mut self, params: &Arc<RtParams>, ramp: usize) {
        // As the process callback does with PipeWire's buffers.
        let wire = |ins: &RtInsert, sends: &[Vec<f32>], returns: &[Vec<f32>]| unsafe {
            for (cell, buf) in ins.send_bufs.iter().zip(sends) {
                cell.set(buf.as_ptr() as *mut f32);
            }
            for (cell, buf) in ins.return_bufs.iter().zip(returns) {
                cell.set(buf.as_ptr());
            }
        };
        for (i, s) in params.strips.iter().enumerate() {
            wire(&s.insert, &self.strip_sends[i], &self.strip_returns[i]);
        }
        for (i, b) in params.buses.iter().enumerate() {
            wire(&b.insert, &self.bus_sends[i], &self.bus_returns[i]);
        }
        let inputs = &self.inputs;
        let outputs = &self.outputs;
        let inp = |s: usize, c: usize| inputs[s][c].as_ptr();
        let out = |b: usize, c: usize| outputs[b][c].as_ptr() as *mut f32;
        unsafe { self.proc_.process(params, self.n, &inp, &out, ramp, 48_000) };
    }

    #[cfg(test)]
    pub fn settle(&mut self, params: &Arc<RtParams>) {
        // ramp of 8 samples, so 4 blocks settle everything.
        for _ in 0..4 {
            self.run(params, 8);
        }
    }
}
