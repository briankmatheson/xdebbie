// SPDX-License-Identifier: GPL-3.0-or-later
//! Regression tests for joining a box to the mesh: box-id assignment,
//! enrolment and approval, the join flow over mTLS, the joiner's generated
//! `[network]`, and `bootstrap --force` keeping what it must keep.
//!
//! Everything runs in-process: a CA box is a real `api::router` behind a
//! real mTLS listener on 127.0.0.1, with a fake netlink (`FakeNetOps`) under
//! its mesh. Nothing here touches libvirt, the host's network or the
//! internet. Discovery, announces and elections are tested elsewhere.

mod box_ids;
mod enrolment;
mod force;
mod join_flow;
mod joiner_config;
mod own_cluster;
mod rejoin;
mod support;
