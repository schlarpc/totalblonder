//! What the driver needs of a device beyond `tb_device::Device`: a wait class per plan, the
//! choice between hybrid and blocking waits, device-side times. Implemented for the two
//! adapters and for [`Guarded`], which adds the check of the host-arena rule to either.

use tb_device::sim::HostGuard;
use tb_device::{ArenaSizes, Completion, Device, DeviceError, Nanos, Plan, Ticket};
use tb_device_gpu::{GpuDevice, Runtime, WaitMode};
use tb_device_ref::RefDevice;

/// A device the tick loop can drive.
pub trait TickDevice: Device {
    /// Queue a plan whose wait is accounted under `class` (`Encoder::wait_class`): a plan of
    /// one class takes about as long from tick to tick, which the hybrid wait relies on.
    fn submit_class(&mut self, plan: &Plan, class: u8) -> Result<Ticket, DeviceError>;
    /// Block in the driver at once instead of the hybrid wait.
    fn set_blocking(&mut self, _blocking: bool) {}
    /// Measure the device-side duration of every plan from now on.
    fn set_timing(&mut self, _on: bool) {}
    /// The device-side duration of the plan waited for last, if it was measured.
    fn device_time(&self, _ticket: Ticket) -> Option<Nanos> {
        None
    }
    /// Whether the adapter's clock is the wall clock (deadlines mean something).
    fn real_time(&self) -> bool;
    /// Whether results appear in the host arena only inside `submit` and `wait`.
    fn synchronous(&self) -> bool;
    /// A line for the report.
    fn describe(&self) -> String;
}

impl<R: Runtime> TickDevice for GpuDevice<R> {
    fn submit_class(&mut self, plan: &Plan, class: u8) -> Result<Ticket, DeviceError> {
        GpuDevice::submit_class(self, plan, class)
    }
    fn set_blocking(&mut self, blocking: bool) {
        self.set_wait_mode(if blocking {
            WaitMode::Blocking
        } else {
            WaitMode::Hybrid
        });
    }
    fn set_timing(&mut self, on: bool) {
        GpuDevice::set_timing(self, on);
    }
    fn device_time(&self, ticket: Ticket) -> Option<Nanos> {
        self.last_device_time()
            .and_then(|(t, d)| (t == ticket).then_some(d))
    }
    fn real_time(&self) -> bool {
        true
    }
    fn synchronous(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        format!(
            "{} device {}, pinned host arena",
            self.runtime(),
            self.name()
        )
    }
}

impl TickDevice for RefDevice {
    fn submit_class(&mut self, plan: &Plan, _class: u8) -> Result<Ticket, DeviceError> {
        self.submit(plan)
    }
    fn real_time(&self) -> bool {
        false
    }
    fn synchronous(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "reference device (the kernels compiled for the host)".to_owned()
    }
}

/// A device with the host-arena rule checked around it (`tb_device::sim::Checked`, with the
/// wait class passed through): a plan that names a host region of a plan in flight is refused,
/// and a region changed while its plan was in flight fails that plan's completion with
/// `DeviceError::BadPlan`. Snapshots every upload's bytes, so it is for tests and debug runs.
#[derive(Debug)]
pub struct Guarded<D> {
    inner: D,
    guard: HostGuard,
}

impl<D: TickDevice> Guarded<D> {
    /// Check the rule around `inner`.
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            guard: HostGuard::new(),
        }
    }
    /// The wrapped device.
    pub fn inner(&self) -> &D {
        &self.inner
    }
    /// The wrapped device.
    pub fn inner_mut(&mut self) -> &mut D {
        &mut self.inner
    }

    fn track(&mut self, ticket: Ticket, plan: &Plan) {
        // After the inner submit: a synchronous device has written its downloads by now.
        let downloads = self.inner.synchronous();
        self.guard.track(ticket, plan, self.inner.host(), downloads);
    }
}

impl<D: TickDevice> Device for Guarded<D> {
    fn arenas(&self) -> ArenaSizes {
        self.inner.arenas()
    }
    fn host(&self) -> &[u8] {
        self.inner.host()
    }
    fn host_mut(&mut self) -> &mut [u8] {
        self.inner.host_mut()
    }
    fn now(&self) -> Nanos {
        self.inner.now()
    }
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError> {
        self.guard.admit(plan)?;
        let ticket = self.inner.submit(plan)?;
        self.track(ticket, plan);
        Ok(ticket)
    }
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion {
        self.guard.check(ticket, self.inner.host());
        let mut c = self.inner.wait(ticket, timeout);
        if c.status == Err(DeviceError::Timeout) {
            return c;
        }
        if let Some(v) = self.guard.release(ticket) {
            c.status = Err(v);
        }
        c
    }
}

impl<D: TickDevice> TickDevice for Guarded<D> {
    fn submit_class(&mut self, plan: &Plan, class: u8) -> Result<Ticket, DeviceError> {
        self.guard.admit(plan)?;
        let ticket = self.inner.submit_class(plan, class)?;
        self.track(ticket, plan);
        Ok(ticket)
    }
    fn set_blocking(&mut self, blocking: bool) {
        self.inner.set_blocking(blocking);
    }
    fn set_timing(&mut self, on: bool) {
        self.inner.set_timing(on);
    }
    fn device_time(&self, ticket: Ticket) -> Option<Nanos> {
        self.inner.device_time(ticket)
    }
    fn real_time(&self) -> bool {
        self.inner.real_time()
    }
    fn synchronous(&self) -> bool {
        self.inner.synchronous()
    }
    fn describe(&self) -> String {
        format!("{}, host-arena rule checked", self.inner.describe())
    }
}
