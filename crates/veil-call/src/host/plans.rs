//! The host signs each epoch's layout ([`CallPlan`]) and sends it, or a proposed one to test,
//! to every member. Only the current host may: after a handover this device signs nothing.

use veil_group::GroupError;
use veil_net::control::Control;
use veil_security::PublicId;
use veil_topology::Plan;

use super::Actor;
use crate::routes::{Directory, Routes};
use crate::{CallError, CallPlan};

impl Actor {
    pub(super) fn publish(&mut self, plan: &Plan, directory: &Directory) -> Result<Routes, CallError> {
        if !self.group.is_host() {
            return Err(CallError::Group(GroupError::NotHost));
        }
        let room_id = self.lobby.room_id();
        let epoch = self.group.epoch();
        let plan = CallPlan::from_plan(plan, directory, room_id, epoch, &self.identity)?;
        let members: Vec<PublicId> = self.group.members().into_iter().map(|(_, d)| d).collect();
        if !plan.covers(&members) {
            return Err(CallError::Plan(
                "the directory does not list this epoch's members",
            ));
        }
        let sealed = plan.seal(&self.identity)?;
        self.latest_plan = Some((epoch, sealed.clone()));
        for device in self.others() {
            self.send_to(&device, Control::Plan(sealed.clone()));
        }
        plan.routes_for(&self.identity.public())
    }

    pub(super) fn probe(
        &mut self,
        plan: &Plan,
        directory: &Directory,
        request: u64,
    ) -> Result<Routes, CallError> {
        if !self.group.is_host() {
            return Err(CallError::Group(GroupError::NotHost));
        }
        let proposed = CallPlan::from_plan(
            plan,
            directory,
            self.lobby.room_id(),
            self.group.epoch(),
            &self.identity,
        )?;
        let members: Vec<PublicId> = self.group.members().into_iter().map(|(_, d)| d).collect();
        if !proposed.covers(&members) {
            return Err(CallError::Plan(
                "the directory does not list this epoch's members",
            ));
        }
        let sealed = proposed.seal(&self.identity)?;
        // Only answers to this request, one per member asked, reach the engine.
        self.checking = Some((request, self.others().into_iter().collect()));
        for device in self.others() {
            self.send_to(
                &device,
                Control::ProbePlan {
                    request,
                    sealed: sealed.clone(),
                },
            );
        }
        proposed.routes_for(&self.identity.public())
    }
}
