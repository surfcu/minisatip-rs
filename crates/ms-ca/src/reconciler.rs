//! Desired programs per CAM versus the programs the CAM was told about.

use std::collections::BTreeMap;

use crate::device::CaDevice;
use crate::program::{Program, ProgramTable};
use crate::{CaDriver, TablesResult};

/// How a program joins and leaves a CAM; one implementation per policy set.
pub trait CaPolicy {
    /// Registers (`update == false`) or refreshes a program on the CAM.
    fn join(
        &mut self,
        dev: &mut CaDevice,
        pmt_id: i32,
        update: bool,
        programs: &ProgramTable,
        driver: &mut dyn CaDriver,
    ) -> TablesResult;
    /// Removes a program from the CAM.
    fn leave(
        &mut self,
        dev: &mut CaDevice,
        pmt_id: i32,
        programs: &ProgramTable,
        driver: &mut dyn CaDriver,
    );
}

/// The C++ behaviour: lazy partner leave, kept-place rejoin, content-hash updates.
#[derive(Clone, Copy, Debug, Default)]
pub struct CppCompatible;

impl CaPolicy for CppCompatible {
    fn join(
        &mut self,
        dev: &mut CaDevice,
        pmt_id: i32,
        update: bool,
        programs: &ProgramTable,
        driver: &mut dyn CaDriver,
    ) -> TablesResult {
        dev.process_pmt(pmt_id, update, programs, driver)
    }

    fn leave(
        &mut self,
        dev: &mut CaDevice,
        pmt_id: i32,
        programs: &ProgramTable,
        driver: &mut dyn CaDriver,
    ) {
        dev.del_pmt(pmt_id, programs, driver);
    }
}

/// What one reconcile step did for a program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Joined(i32, TablesResult),
    Updated(i32, TablesResult),
    Left(i32),
}

/// One CAM driven towards a desired program list.
pub struct Reconciler<P: CaPolicy> {
    pub policy: P,
    pub device: CaDevice,
    /// Programs registered on the CAM, with the content last pushed, in join order.
    actual: Vec<(i32, Program)>,
    /// Programs that last answered `Retry`.
    pending: BTreeMap<i32, Program>,
    /// Programs refused with `NoRetry` (`disabled_ca_mask`), until their content changes.
    rejected: BTreeMap<i32, Program>,
}

impl<P: CaPolicy> Reconciler<P> {
    pub fn new(policy: P, device: CaDevice) -> Self {
        Self {
            policy,
            device,
            actual: Vec::new(),
            pending: BTreeMap::new(),
            rejected: BTreeMap::new(),
        }
    }

    /// Program ids currently registered.
    pub fn actual(&self) -> Vec<i32> {
        self.actual.iter().map(|(id, _)| *id).collect()
    }

    /// Leaves first (in join order), then joins and updates in `desired` order.
    pub fn reconcile(
        &mut self,
        desired: &[i32],
        programs: &ProgramTable,
        driver: &mut dyn CaDriver,
    ) -> Vec<Step> {
        let mut steps = Vec::new();
        let leaving: Vec<i32> = self
            .actual
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !desired.contains(id))
            .collect();
        for id in leaving {
            self.policy.leave(&mut self.device, id, programs, driver);
            self.actual.retain(|(a, _)| *a != id);
            steps.push(Step::Left(id));
        }
        self.pending.retain(|id, _| desired.contains(id));
        self.rejected.retain(|id, _| desired.contains(id));
        for &id in desired {
            let Some(current) = programs.get(id) else {
                continue;
            };
            match self.actual.iter().position(|(a, _)| *a == id) {
                Some(i) if self.actual[i].1 != *current => {
                    let r = self
                        .policy
                        .join(&mut self.device, id, true, programs, driver);
                    self.actual[i].1 = current.clone();
                    steps.push(Step::Updated(id, r));
                }
                Some(_) => {}
                None if self.rejected.get(&id) == Some(current) => {}
                None => {
                    let r = self
                        .policy
                        .join(&mut self.device, id, false, programs, driver);
                    match r {
                        TablesResult::Ok => {
                            self.pending.remove(&id);
                            self.actual.push((id, current.clone()));
                        }
                        TablesResult::Retry => {
                            self.pending.insert(id, current.clone());
                        }
                        TablesResult::NoRetry => {
                            self.rejected.insert(id, current.clone());
                        }
                    }
                    steps.push(Step::Joined(id, r));
                }
            }
        }
        steps
    }

    /// Programs waiting for a retry (CAM busy or initializing).
    pub fn pending(&self) -> Vec<i32> {
        self.pending.keys().copied().collect()
    }
}
