// SPDX-License-Identifier: MIT
// Adapted from Anomaly Rune Kit, revision d03349e25189ce1edc8c26ada6bf6d4aac72c826.
// Copyright 2026 Anomaly. See LICENSE-MIT.
//! A diff view for reviewing changes: files of hunks, unified or split, with word changes,
//! syntax colours, expandable unchanged ranges, line annotations and line selection, on a
//! virtual list that keeps a fifty-thousand-line diff at frame rate.
//!
//! [`parse`] reads a patch (`git diff`) and [`FileDiff::from_texts`] diffs two texts. The app
//! keeps a [`DiffState`] of the files in its state and draws it with [`DiffView`].

// Retain the upstream component API and its tests for future dependency updates.
#![allow(dead_code, unused_imports)]

mod model;
mod myers;
mod state;
mod view;

pub use model::{FileDiff, FileStatus, Hunk, Line, LineKind, Side, parse};
pub use state::{Annotation, DiffMode, DiffState, EXPAND_STEP, Expand, LineRange, LineRef};
pub use view::DiffView;
