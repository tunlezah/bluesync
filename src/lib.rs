//! SoundSync — Bluetooth A2DP sink rebuild (foundation library).
//!
//! Foundation modules reproduce load-bearing nuances from the legacy app
//! (see research/audio-routing-nuances.md). They are pure and unit-tested.

pub mod artwork;
pub mod audio;
pub mod bluetooth;
pub mod capabilities;
pub mod cast;
pub mod cli;
pub mod config;
pub mod config_store;
pub mod daemon;
pub mod doctor;
pub mod dsp;
pub mod install;
pub mod output;
pub mod state;
pub mod sys;
pub mod web;
pub mod wireplumber;
