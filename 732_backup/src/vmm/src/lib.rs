// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause
//! Reference VMM built with rust-vmm components and minimal glue.
#![allow(missing_docs)]
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use core::num;
use std::borrow::Borrow;
use std::convert::{TryFrom, TryInto};

use serde::{Serialize, Deserialize};
use std::net::{TcpListener, TcpStream};

#[cfg(target_arch = "aarch64")]
use std::convert::TryInto;
use std::fs::File;
use std::io::{self, stdin, stdout, Read, BufReader};
use std::ops::DerefMut;
use std::path::{PathBuf, Path};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};
use std::sync::mpsc::{Sender, Receiver};
use std::sync::mpsc;
use std::process::Command;

use std::fs;

use std::io::{BufWriter, Write};



use event_manager::{EventManager, EventOps, Events, MutEventSubscriber, SubscriberOps};
use kvm_bindings::KVM_API_VERSION;
use kvm_ioctls::{
    Cap::{self, Ioeventfd, Irqchip, Irqfd, UserMemory},
    Kvm,
};
use libc::SYS_nanosleep;
use linux_loader::cmdline;
#[cfg(target_arch = "x86_64")]
use linux_loader::configurator::{
    self, linux::LinuxBootConfigurator, BootConfigurator, BootParams,
};
#[cfg(target_arch = "x86_64")]
use linux_loader::{bootparam::boot_params, cmdline::Cmdline};
// use serial::{IER_RDA_OFFSET,IER_RDA_OFFSET};
use linux_loader::loader::{self, KernelLoader, KernelLoaderResult};
#[cfg(target_arch = "x86_64")]
use linux_loader::loader::{
    bzimage::BzImage,
    elf::{self, Elf},
    load_cmdline,
};
use vm_device::bus::{MmioAddress, MmioRange};
#[cfg(target_arch = "x86_64")]
use vm_device::bus::{PioAddress, PioRange};
use vm_device::device_manager::IoManager;
#[cfg(target_arch = "aarch64")]
use vm_device::device_manager::MmioManager;
#[cfg(target_arch = "x86_64")]
use vm_device::device_manager::PioManager;
#[cfg(target_arch = "aarch64")]
use vm_memory::GuestMemoryRegion;
use vm_memory::{GuestAddress, GuestMemory, GuestMemoryMmap, Bytes};
#[cfg(target_arch = "x86_64")]
use vm_superio::I8042Device;
#[cfg(target_arch = "aarch64")]
use vm_superio::Rtc;
use vm_superio::Serial;
use vmm_sys_util::signal::{Killable, SIGRTMIN};
// use libc::SIGRTMIN;

use vmm_sys_util::{epoll::EventSet, eventfd::EventFd, terminal::Terminal};

#[cfg(target_arch = "x86_64")]
use boot::build_bootparams;
pub use config::*;
use devices::virtio::block::{self, BlockArgs};
use devices::virtio::net::{self, NetArgs};
use devices::virtio::{Env, MmioConfig};
pub mod dedup;
pub mod memory_snapshot;

use crate::memory_snapshot::{GuestMemoryRegionState, GuestMemoryState, SnapshotMemory};
use crate::dedup::DedupManager;
// use memory_snapshot::{GuestMemoryRegionState, GuestMemoryState};
#[cfg(target_arch = "x86_64")]
use devices::legacy::I8042Wrapper;
#[cfg(target_arch = "aarch64")]
use devices::legacy::RtcWrapper;
use devices::legacy::{EventFdTrigger, SerialWrapper};
use vm_vcpu::vm::{self, ExitHandler, KvmVm, VmConfig, VmRunState, VmState};

#[cfg(target_arch = "aarch64")]
use arch::{FdtBuilder, AARCH64_FDT_MAX_SIZE, AARCH64_MMIO_BASE, AARCH64_PHYS_MEM_START};

mod boot;
mod config;

use versionize::{VersionMap, Versionize};

/// First address past 32 bits is where the MMIO gap ends.
pub(crate) const MMIO_GAP_END: u64 = 1 << 32;
/// Size of the MMIO gap.
pub(crate) const MMIO_GAP_SIZE: u64 = 768 << 20;
/// The start of the MMIO gap (memory area reserved for MMIO devices).
pub(crate) const MMIO_GAP_START: u64 = MMIO_GAP_END - MMIO_GAP_SIZE;
/// Address of the zeropage, where Linux kernel boot parameters are written.
#[cfg(target_arch = "x86_64")]
const ZEROPG_START: u64 = 0x7000;
/// Address where the kernel command line is written.
#[cfg(target_arch = "x86_64")]
const CMDLINE_START: u64 = 0x0002_0000;
// there is some pending data to be processed.

pub const IER_RDA_BIT: u8 = 0b0000_0001;
// Received Data Available interrupt offset
pub const IER_RDA_OFFSET: u16 = 1;

/// Default high memory start (1 MiB).
#[cfg(target_arch = "x86_64")]
pub const DEFAULT_HIGH_RAM_START: u64 = 0x0010_0000;

/// Default address for loading the kernel.
#[cfg(target_arch = "x86_64")]
pub const DEFAULT_KERNEL_LOAD_ADDR: u64 = DEFAULT_HIGH_RAM_START;
#[cfg(target_arch = "aarch64")]
/// Default address for loading the kernel.
pub const DEFAULT_KERNEL_LOAD_ADDR: u64 = AARCH64_PHYS_MEM_START;

/// Default kernel command line.
#[cfg(target_arch = "x86_64")]
pub const DEFAULT_KERNEL_CMDLINE: &str = "panic=1 pci=off";
#[cfg(target_arch = "aarch64")]
/// Default kernel command line.
pub const DEFAULT_KERNEL_CMDLINE: &str = "reboot=t panic=1 pci=off";

const CHUNK_SIZE: usize = 1024 *1024;

// constants: database path, state path, map1 path, map2 path
const DATABASE_PATH: &str = "./database/";
// state path is database path + state
const STATE_PATH: &str = "./database/state";
// map1 path is database path + map1
const MAP1_PATH: &str = "./database/map1";
// map2 path is database path + map2
const MAP2_PATH: &str = "./database/map2";

/// VMM memory related errors.
#[derive(Debug)]
pub enum MemoryError {
    /// Not enough memory slots.
    NotEnoughMemorySlots,
    /// Failed to configure guest memory.
    VmMemory(vm_memory::Error),
}

/// VMM errors.
#[derive(Debug)]
pub enum Error {
    /// Failed to create block device.
    Block(block::Error),
    /// Failed to write boot parameters to guest memory.
    #[cfg(target_arch = "x86_64")]
    BootConfigure(configurator::Error),
    /// Error configuring boot parameters.
    #[cfg(target_arch = "x86_64")]
    BootParam(boot::Error),
    /// Error configuring the kernel command line.
    Cmdline(cmdline::Error),
    /// Error setting up the serial device.
    SerialDevice(devices::legacy::SerialError),
    /// Event management error.
    EventManager(event_manager::Error),
    /// I/O error.
    IO(io::Error),
    /// Failed to load kernel.
    KernelLoad(loader::Error),
    /// Failed to create net device.
    Net(net::Error),
    /// Address stored in the rip registry does not fit in guest memory.
    RipOutOfGuestMemory,
    /// Invalid KVM API version.
    KvmApiVersion(i32),
    /// Unsupported KVM capability.
    KvmCap(Cap),
    /// Error issuing an ioctl to KVM.
    KvmIoctl(kvm_ioctls::Error),
    /// Memory error.
    Memory(MemoryError),
    /// VM errors.
    Vm(vm::Error),
    /// Exit event errors.
    ExitEvent(io::Error),
    #[cfg(target_arch = "x86_64")]
    /// Cannot retrieve the supported MSRs.
    GetSupportedMsrs(vm_vcpu_ref::x86_64::msrs::Error),
    #[cfg(target_arch = "aarch64")]
    /// Cannot setup the FDT for booting.
    SetupFdt(arch::Error),
}

impl std::convert::From<vm::Error> for Error {
    fn from(vm_error: vm::Error) -> Self {
        Self::Vm(vm_error)
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MigrationMessage {
    pub data: Vec<u8>,
    pub data_len: usize,
    pub dirty_pages: Vec<u64>,
    pub dirty_pages_len: usize,
    pub cpu_state_file_path: String,
    pub is_last: bool,
    pub init_migration: bool
} 

static MIGRATION_PORT: i32 = 1989;



/// Dedicated [`Result`](https://doc.rust-lang.org/std/result/) type.
pub type Result<T> = std::result::Result<T, Error>;

type Block = block::Block<Arc<GuestMemoryMmap>>;
type Net = net::Net<Arc<GuestMemoryMmap>>;

pub struct RpcController {
    pub event_fd: EventFd,
    pub pause_or_resume: AtomicU16,
    pub cpu_snapshot_path: String,
    pub memory_snapshot_path: String,
}

impl RpcController {
    fn new() -> RpcController {
        RpcController {
            event_fd: EventFd::new(libc::EFD_NONBLOCK)
                .map_err(Error::ExitEvent)
                .unwrap(),
            pause_or_resume: AtomicU16::new(0),
            cpu_snapshot_path: "".to_string(),
            memory_snapshot_path: "".to_string(),
            // 0 mean nothing, 1 mean pause, 2 mean resume.
        }
    }
    fn which_event(&self) -> &'static str {
        let val = self.pause_or_resume.load(Ordering::Acquire);
        if val == 1 {
            return "PAUSE";
        } else if val == 2 {
            return "RESUME";
        }
        "5 star"
    }
}

impl MutEventSubscriber for RpcController {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        if events.event_set().contains(EventSet::IN) {
            // do nothing eat 5 star.
        }
        if events.event_set().contains(EventSet::ERROR) {
            // We cannot do much about the error (besides log it).
            // TODO: log this error once we have a logger set up.
            let _ = ops.remove(Events::new(&self.event_fd, EventSet::IN));
        }
    }
    fn init(&mut self, ops: &mut EventOps) {
        ops.add(Events::new(&self.event_fd, EventSet::IN))
            .expect("Cannot initialize exit handler.");
    }
}

/// A live VMM.
pub struct Vmm {
    pub vm: KvmVm<WrappedExitHandler>,
    pub kernel_cfg: KernelConfig,
    pub guest_memory: GuestMemoryMmap,
    // The `device_mgr` is an Arc<Mutex> so that it can be shared between
    // the Vcpu threads, and modified when new devices are added.
    pub device_mgr: Arc<Mutex<IoManager>>,
    // Arc<Mutex<>> because the same device (a dyn DevicePio/DeviceMmio from IoManager's
    // perspective, and a dyn MutEventSubscriber from EventManager's) is managed by the 2 entities,
    // and isn't Copy-able; so once one of them gets ownership, the other one can't anymore.
    pub event_mgr: EventManager<Arc<Mutex<dyn MutEventSubscriber + Send>>>,
    pub exit_handler: WrappedExitHandler,
    pub block_devices: Vec<Arc<Mutex<Block>>>,
    pub net_devices: Vec<Arc<Mutex<Net>>>,
    pub rpc_controller: Arc<Mutex<RpcController>>,
    // TODO: fetch the vcpu number from the `vm` object.
    // TODO-continued: this is needed to make the arm POC work as we need to create the FDT
    // TODO-continued: after the other resources are created.
    #[cfg(target_arch = "aarch64")]
    pub num_vcpus: u64,
    pub is_resume: bool,
    pub start_migration_thread: bool,
    pub dedup_mgr: DedupManager,
    // pub kvm: Kvm
}

// The `VmmExitHandler` is used as the mechanism for exiting from the event manager loop.
// The Vm is notifying us through the `kick` method when it exited. Once the Vm finished
// the execution, it is time for the event manager loop to also exit. This way, we can
// terminate the VMM process cleanly.
struct VmmExitHandler {
    exit_event: EventFd,
    keep_running: AtomicBool,
}

// /// Dumps all contents of GuestMemoryMmap to a writer.
// fn dump<T: std::io::Write>( guest_memory: &GuestMemoryMmap , writer: &mut T)  {
//     guest_memory.iter()
//         .try_for_each(|region| {
//             region.write_all_to(MemoryRegionAddress(0), writer, region.len() as usize)
//         }).unwrap();
//         // .map_err(Error::)
// }

fn get_memory_state(size: usize) -> GuestMemoryState {
    let region_state = GuestMemoryRegionState {
        base_address: 0,
        size: size,
        offset: 0,
    };

    GuestMemoryState {
        regions: vec![region_state],
    }
}

// fn restore(file: File, size: usize) -> GuestMemoryMmap {
//     let ranges = vec![
//         (GuestAddress(0), size, Some(FileOffset::new(file, 0))),
//     ];

//     GuestMemoryMmap::from_ranges_with_files(ranges).unwrap()
// }
// The wrapped exit handler is needed because the ownership of the inner `VmmExitHandler` is
// shared between the `KvmVm` and the `EventManager`. Clone is required for implementing the
// `ExitHandler` trait.
#[derive(Clone)]
pub struct WrappedExitHandler(Arc<Mutex<VmmExitHandler>>);

impl WrappedExitHandler {
    fn new() -> Result<WrappedExitHandler> {
        Ok(WrappedExitHandler(Arc::new(Mutex::new(VmmExitHandler {
            exit_event: EventFd::new(libc::EFD_NONBLOCK).map_err(Error::ExitEvent)?,
            keep_running: AtomicBool::new(true),
        }))))
    }

    fn keep_running(&self) -> bool {
        self.0.lock().unwrap().keep_running.load(Ordering::Acquire)
    }
}

impl ExitHandler for WrappedExitHandler {
    fn kick(&self) -> io::Result<()> {
        self.0.lock().unwrap().exit_event.write(1)
    }
}

impl MutEventSubscriber for VmmExitHandler {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        if events.event_set().contains(EventSet::IN) {
            self.keep_running.store(false, Ordering::Release);
        }
        if events.event_set().contains(EventSet::ERROR) {
            // We cannot do much about the error (besides log it).
            // TODO: log this error once we have a logger set up.
            let _ = ops.remove(Events::new(&self.exit_event, EventSet::IN));
        }
    }

    fn init(&mut self, ops: &mut EventOps) {
        ops.add(Events::new(&self.exit_event, EventSet::IN))
            .expect("Cannot initialize exit handler.");
    }
}

impl TryFrom<VMMConfig> for Vmm {
    type Error = Error;

    fn try_from(config: VMMConfig) -> Result<Self> {
        let kvm = Kvm::new().map_err(Error::KvmIoctl)?;

        // Check that the KVM on the host is supported.
        let kvm_api_ver = kvm.get_api_version();
        if kvm_api_ver != KVM_API_VERSION as i32 {
            return Err(Error::KvmApiVersion(kvm_api_ver));
        }
        Vmm::check_kvm_capabilities(&kvm)?;

        // NOTE: RPC event controller
        let rpc_controller = Arc::new(Mutex::new(RpcController::new()));

        let device_mgr = Arc::new(Mutex::new(IoManager::new()));

        // Create the KvmVm.
        let vm_config = VmConfig::new(&kvm, config.vcpu_config.num, config.kernel_config.clone().starter_path)?;
        
        let wrapped_exit_handler = WrappedExitHandler::new()?;

        let mut event_manager = EventManager::<Arc<Mutex<dyn MutEventSubscriber + Send>>>::new()
            .map_err(Error::EventManager)?;
        event_manager.add_subscriber(wrapped_exit_handler.0.clone());

        // NOTE: Register of rpc controller.
        event_manager.add_subscriber(rpc_controller.clone());

        // save vm
        // let my_vmstate = vm.save_state().unwrap();
        // Self::save_cpu("cc.txt", &my_vmstate);


        let mut start_migrating_thread = true;

        let guest_memory;
        let mut is_resume = false;
        let mem_size = ((config.memory_config.size_mib as u64) << 20) as usize;

        let dedup_mgr : DedupManager = DedupManager{
            CHUNK_SIZE,
            DATABASE_PATH: DATABASE_PATH.to_string(),
            STATE_PATH: STATE_PATH.to_string(),
            MAP1_PATH: MAP1_PATH.to_string(),
            MAP2_PATH: MAP2_PATH.to_string()
        };

        let my_vm = if config.snapshot_config.is_none() {
            let mem_regions = vec![(None, GuestAddress(0), mem_size)];
            guest_memory = vm_memory::create_guest_memory(&mem_regions[..], true).unwrap();
            KvmVm::new(
                &kvm,
                vm_config,
                &guest_memory,
                wrapped_exit_handler.clone(),
                device_mgr.clone(),
            )
            .unwrap()
        } else {
            // resume
            is_resume = true;
            let memory_snapshot_path = config.snapshot_config.clone().unwrap().memory_snapshot_path;
            let cpu_snapshot_path = config.snapshot_config.unwrap().cpu_snapshot_path;

            // println!("restoring snapshot");

            // guest_memory = GuestMemoryMmap::restore(Some(memory_file.as_file()), &memory_state, false);

            let vmstate;

            if !config.migrating {
                vmstate = Self::restore_cpu(&cpu_snapshot_path[..]);
                
                let memory_state = get_memory_state(mem_size);
                    dedup_mgr.load_file(&memory_snapshot_path);
                    let file = File::options()
                        .write(true)
                        .read(true)
                        .open(memory_snapshot_path)
                        .unwrap();
                guest_memory = GuestMemoryMmap::restore(Some(&file), &memory_state, false);
                // println!("snapshot restored");
            } 
            else {
                println!("restoring after migration....");

                let mem_regions = vec![(None, GuestAddress(0), mem_size)];
                guest_memory = vm_memory::create_guest_memory(&mem_regions[..], true).unwrap();

                let mut cpu_live_migration_snapshot_path = "cpu_live.txt".to_string();


                let addr = "0.0.0.0:".to_string() + &MIGRATION_PORT.to_string();
                let mut migrator_conn = TcpStream::connect(addr).unwrap();

                let mut itr = 0;


                loop {
                    println!("Migration: itr = {}", itr);
                    
                    let buf = &mut [0; 8];

                    migrator_conn.read_exact(buf).unwrap();

                    let data_len = u64::from_le_bytes(*buf);
                    

                    println!("data_len: {}", data_len);    

                    let mut data_buf : Vec<u8> = vec![0; data_len as usize];

                    migrator_conn.read_exact(&mut data_buf).unwrap();

                    println!("Received data");

                    let mut done = false;


                    if itr == 0 {
                        // first itr directly sends all the guest memory(unserialized)
                        let num_pages = data_len / 4096;

                        for i in 0..num_pages {
                            let page_addr = i * 4096;
                            let page = &data_buf[page_addr as usize..(page_addr + 4096) as usize];
                            guest_memory.write_slice(page, GuestAddress(i * 4096)).unwrap();
                        }

                    }
                    else {

                        let migration_msg : MigrationMessage = bincode::deserialize(&data_buf).unwrap();

                        let dirty_pages_data = migration_msg.data;

                        println!("num dirty pages: {}", migration_msg.dirty_pages_len);

                        done = migration_msg.is_last;

                        for i in 0..migration_msg.dirty_pages_len {
                            let page_num = migration_msg.dirty_pages[i];
                            let page_addr = page_num * 4096;
                            let page = &dirty_pages_data[i * 4096..(i + 1) * 4096];
                            guest_memory.write_slice(page, GuestAddress(page_addr)).unwrap();
                        }
                        cpu_live_migration_snapshot_path = migration_msg.cpu_state_file_path;

                        // print sha hash of first dirty page
                        // if migration_msg.dirty_pages_len > 0 {
                        //     let page_num = migration_msg.dirty_pages[0];
                        //     println!("first dirty page.. {}", page_num);
                        //     let page_addr = page_num * 4096;
                        //     let page_recvd = &dirty_pages_data[0..4096];
                        //     let page_recvd_hash = sha256::digest(page_recvd);
                            
                        //     let mut page_applied: Vec<u8> = vec![0; 4096];

                        //     guest_memory.read_slice(&mut page_applied[..], GuestAddress(page_addr)).unwrap();
                            
                        //     let page_applied_hash = sha256::digest(&page_applied[..]);

                        //     assert_eq!(page_recvd_hash, page_applied_hash);

                        //     println!("first dirty page hash: {}", page_recvd_hash);
                        // }


                    }

                    // write the guest mem to file for debugging


                    if done {
                        break;
                    }

                    itr += 1;
                }

                println!("restored memory");

                
                println!("restoring cpu from: {}", cpu_live_migration_snapshot_path);

                vmstate = Self::restore_cpu(&cpu_live_migration_snapshot_path[..]);


                start_migrating_thread = false;

            }

            KvmVm::from_state(
                &kvm,
                vmstate,
                &guest_memory,
                wrapped_exit_handler.clone(),
                device_mgr.clone(),
            )
            .unwrap()
        };

        

        let mut vmm = Vmm {
            vm: my_vm,
            guest_memory,
            device_mgr: device_mgr.clone(),
            event_mgr: event_manager,
            kernel_cfg: config.kernel_config,
            exit_handler: wrapped_exit_handler.clone(),
            block_devices: Vec::new(),
            net_devices: Vec::new(),
            rpc_controller,
            #[cfg(target_arch = "aarch64")]
            num_vcpus: config.vcpu_config.num as u64,
            is_resume: is_resume,
            dedup_mgr: dedup_mgr,
            start_migration_thread: start_migrating_thread
            // kvm: kvm
        };

        // println!("vcpu state: {:?}", vmm.vm.vcpus[0].run_state.vm_state.lock().unwrap());

        // INFERENCE: snapshot restored after add_serial_console won't work
        vmm.add_serial_console()?;
        #[cfg(target_arch = "x86_64")]
        vmm.add_i8042_device()?;
        #[cfg(target_arch = "aarch64")]
        vmm.add_rtc_device();

        // Adding the virtio devices. We'll come up with a cleaner abstraction for `Env`.
        if let Some(cfg) = config.block_config.as_ref() {
            vmm.add_block_device(cfg)?;
        }

        if let Some(cfg) = config.net_config.as_ref() {
            vmm.add_net_device(cfg)?;
        }

        // if is_resume {
        //     println!("reactivating net device");
        //     vmm.net_devices.get(0).unwrap().lock().unwrap().act();
        // }

        // vmm.emulate_serial_init();
        Ok(vmm)
    }
}

impl Vmm {
    /// Sets RDA bit in serial console
    pub fn emulate_serial_init(&self) -> Result<()> {
        #[cfg(target_arch = "x86_64")]
        let serial = self
            .device_mgr // replacement for pio device manager
            //                .stdio_serial
            .lock()
            .expect("Poisoned lock");

        // When restoring from a previously saved state, there is no serial
        // driver initialization, therefore the RDA (Received Data Available)
        // interrupt is not enabled. Because of that, the driver won't get
        // notified of any bytes that we send to the guest. The clean solution
        // would be to save the whole serial device state when we do the vm
        // serialization. For now we set that bit manually
        serial
            // .serial
            .pio_write(PioAddress(IER_RDA_OFFSET), &[IER_RDA_BIT])
            .unwrap();
        // .map_err(|_| Error::Serial(std::io::Error::last_os_error()))?;
        Ok(())
    }
    ///
    pub fn save_snapshot(
        &mut self,
        cpu_snapshot_path: String,
        memory_snapshot_path: String,
        resume: bool,
    ) {
        if resume {
            // self.vm.snapshot_and_resume(cpu_snapshot_path, memory_snapshot_path);
            self.snapshot_and_resume(&cpu_snapshot_path[..], &memory_snapshot_path[..]);
        } else {
            // self.vm.snapshot_and_pause(cpu_snapshot_path, memory_snapshot_path);
            self.snapshot_and_pause(&cpu_snapshot_path[..], &memory_snapshot_path[..]);
        }
    }

    pub fn snapshot_and_resume(&mut self, snapshot_path: &str, memory_snapshot_path: &str) {
        // NOTE: 1. Kicking all the vcpus out of their run loop in suspending state
        self.vm
            .vcpu_run_state
            .set_and_notify(VmRunState::Suspending);
        for handle in self.vm.vcpu_handles.iter() {
            let _ = handle.kill(SIGRTMIN() + 0);
        }

        for i in 0..self.vm.config.num_vcpus {
            let r = self.vm.vcpu_rx.as_ref().unwrap();
            r.recv().unwrap();
            println!("Received message from {i}th cpu");
        }

        let vm_state = self.vm.save_state().unwrap();
        Self::take_snapshot(
            snapshot_path,
            memory_snapshot_path,
            &vm_state,
            &self.guest_memory,
            &self.dedup_mgr,
            true
        );

        // NOTE: 4. Set and notify all vcpus to Running state so that they breaks out of their wait loop and resumes
        self.vm.vcpu_run_state.set_and_notify(VmRunState::Running);
        // self.vm.run(Some(GuestAddress(0)), true).unwrap();
    }


    // pub fn pause_and_save_cpus(&mut self, snapshot_path: &str) {
    //     self.vm.vcpu_run_state.set_and_notify(VmRunState::Exiting);
    //     for handle in self.vm.vcpu_handles.iter() {
    //         let _ = handle.kill(SIGRTMIN() + 0);
    //     }
    //     for i in 0..self.vm.config.num_vcpus {
    //         let r = self.vm.vcpu_rx.as_ref().unwrap();
    //         match r.recv() {
    //             Ok(_) => {}
    //             Err(e) => {
    //                 println!("Error:{:?}", e);
    //             }
    //         }
    //         println!("Received message from {i}th cpu");
    //     }
    //     let vm_state = self.vm.save_state().unwrap();

    //     Self::save_cpu(snapshot_path, &vm_state);

    //             // std::fs::copy("memory.txt", memory_path);
    //     let mut writer = File::options()
    //             .read(true)
    //             .write(true)
    //             .create(true)
    //             .open("mem_snap.txt")
    //             .unwrap();

    //         SnapshotMemory::dump(&self.guest_memory, &mut writer);
    //         writer.flush().unwrap();
    //         writer.sync_all().unwrap();
    //         self.dedup_mgr.save_file("mem_snap.txt");

    //     // Now, make the vmm exit out of run loop
    //     let _ = self.vm.exit_handler.kick();
        
    // }


    pub fn snapshot_and_pause(&mut self, snapshot_path: &str, memory_snapshot_path: &str) {
        // NOTE: 1. Kicking all the vcpus out of their run loop in suspending state
        self.vm.vcpu_run_state.set_and_notify(VmRunState::Exiting);
        for handle in self.vm.vcpu_handles.iter() {
            let _ = handle.kill(SIGRTMIN() + 0);
        }

        for i in 0..self.vm.config.num_vcpus {
            let r = self.vm.vcpu_rx.as_ref().unwrap();
            match r.recv() {
                Ok(_) => {}
                Err(e) => {
                    println!("Error:{:?}", e);
                }
            }
            println!("Received message from {i}th cpu");
        }

        let vm_state = self.vm.save_state().unwrap();
        Self::take_snapshot(
            snapshot_path,
            memory_snapshot_path,
            &vm_state,
            &self.guest_memory,
            &self.dedup_mgr,
            false
        );

        // Now, make the vmm exit out of run loop
        let _ = self.vm.exit_handler.kick();
    }

    pub fn take_snapshot(
        snapshot_path: &str,
        memory_path: &str,
        vm_state: &VmState,
        guest_memory: &GuestMemoryMmap,
        dedup_mgr: &DedupManager,
        save_mem: bool
    ) {
        Self::save_cpu(snapshot_path, vm_state);

        if save_mem {
            println!("Dedup saving memory");
            // std::fs::copy("memory.txt", memory_path);
            let mut writer = File::options()
                .read(true)
                .write(true)
                .create(true)
                .open(memory_path)
                .unwrap();
            SnapshotMemory::dump(guest_memory, &mut writer);
            writer.flush().unwrap();
            writer.sync_all().unwrap();
            dedup_mgr.save_file(memory_path);
            println!("deduped memory snapshot done");
        }
    }

    ///
    pub fn save_cpu(snapshot_path: &str, vm_state: &VmState) {
        let mut snapshot_file = File::create(snapshot_path).unwrap();
        let mut mem = Vec::new();
        let version_map = VersionMap::new();
        vm_state.serialize(&mut mem, &version_map, 1).unwrap();
        snapshot_file.write_all(&mem).unwrap();
    }

    /// restore cpu
    pub fn restore_cpu(snapshot_path: &str) -> VmState {
        let mut snapshot_file = File::open(snapshot_path).unwrap();
        let version_map = VersionMap::new();
        let mut bytes = Vec::new();
        snapshot_file.read_to_end(&mut bytes).unwrap();
        VmState::deserialize(&mut bytes.as_slice(), &version_map, 1).unwrap()
    }

    /// Run the VMM.
    pub fn run(&mut self) -> Result<()> {
        println!("Running VMM");
        let kernel_load_addr;
        if !self.is_resume {
            let load_result = self.load_kernel()?;
            kernel_load_addr = self.compute_kernel_load_addr(&load_result)?;
            // let kernel_load_addr = GuestAddress(1049088);
            if stdin().lock().set_raw_mode().is_err() {
                eprintln!("Failed to set raw mode on terminal. Stdin will echo.");
            }
        } else {
            kernel_load_addr = GuestAddress(0);
        }
        
        println!("resuming..? {}", self.is_resume);
        // println!("FLOW: Starting VM");
        self.vm
            .run(Some(kernel_load_addr), self.is_resume)
            .map_err(Error::Vm)?;

        

        let (migration_save_done_tx, migration_save_done_rx) : (Sender<i32>, Receiver<i32>) = mpsc::channel();

        let (migration_save_do_tx, migration_save_do_rx) : (Sender<i32>, Receiver<i32>) = mpsc::channel();

        let (exit_vmm_tx, exit_vmm_rx) : (Sender<i32>, Receiver<i32>) = mpsc::channel();


        if self.start_migration_thread {
            self.live_migrate(migration_save_do_tx, migration_save_done_rx, exit_vmm_tx);
        }


        loop {
            match self.event_mgr.run() {
                Ok(n) => {
                    // println!("dispatched {} events", n);
                }
                Err(e) => eprintln!("Failed to handle events: {:?}", e),
            }
            // NOTE: checking if need to snapshot or not
            let rpc = self.rpc_controller.clone();
            let rpc_controller = rpc.lock().unwrap();
            let cpu_snapshot_path = rpc_controller.cpu_snapshot_path.clone();
            let memory_snapshot_path = rpc_controller.memory_snapshot_path.clone();

            match rpc_controller.which_event() {
                "PAUSE" => {
                    self.save_snapshot(cpu_snapshot_path, memory_snapshot_path, false);
                    rpc_controller.pause_or_resume.store(0, Ordering::Relaxed);
                    if self.start_migration_thread {
                        exit_vmm_rx.recv().unwrap();
                    }
                }
                "RESUME" => {
                    self.save_snapshot(cpu_snapshot_path, memory_snapshot_path, true);
                    rpc_controller.pause_or_resume.store(0, Ordering::Relaxed);
                }
                _ => {
                    // do nothing, eat 5 star.
                }
            }

            if !self.exit_handler.keep_running() {
                break;
            }
        }

        println!("FLOW: VM stopped");
        self.vm.shutdown();

        Ok(())
    }

    fn live_migrate(&mut self, cpu_save_do: Sender<i32>, cpu_save_done: Receiver<i32>, exit_vmm: Sender<i32>) {


        let mem_size = usize::try_from(self.guest_memory.last_addr().0 + 1).unwrap();

        let vm_fd = self.vm.vm_fd().clone();

        let guest_memory = self.guest_memory.clone();

            let _ = std::thread::spawn(move || {

                let addr = "0.0.0.0:".to_string() + &MIGRATION_PORT.to_string();

                println!("Waiting for migration request on {}", addr);


                let listener = TcpListener::bind(addr).unwrap();
        
                let mut migrate_host = listener.accept().unwrap().0;
        
                println!("Recevied migration request, Initializing migration...");


                let mut buf = vec![0; mem_size];

                let mut migration_itr = 0;

                let mut cpu_snap_path = "";


                let mut dirty_pages_in_iters: Vec<i64> = vec![];

                let mut last_itr = -1;

        
                loop {
                    
                    let dirty_pages_bitmap = vm_fd.get_dirty_log(0, mem_size).unwrap();


                    let mut dirty_pages = vec![];

                    for page in 0..dirty_pages_bitmap.len() {
                        let dirty_bit = dirty_pages_bitmap.get(page).unwrap();
                        // if dirty bit is set increase count
                        
                        // iterate over set ones
                        for i in 0..64 {
                            if (*dirty_bit & (1 << i)) != 0 {
                                dirty_pages.push(page * 64 + i);
                            }
                        }

                    }

                    // println!("migration itr: {}, dirty pages: {}",migration_itr, dirty_pages.len());

                    assert!(guest_memory.regions.len() == 1);

                    let region = guest_memory.regions.get(0).unwrap();
                    
                    if migration_itr == 0 {
                        let start_addr = vm_memory::MemoryRegionAddress(0x0);
                        region.read(&mut buf, start_addr).unwrap();

                        let mut dpages = vec![];
                        for i in 0..dirty_pages_bitmap.len() * 64 {
                            dpages.push(i as u64);
                        }

                        dirty_pages_in_iters.push(dpages.len().try_into().unwrap());


                        let send_data = &buf[..];

                        // let migration_message = MigrationMessage {
                        //     data: send_data.to_vec(),
                        //     data_len: send_data.len(),
                        //     dirty_pages_len: dpages.len(),
                        //     dirty_pages: dpages,
                        //     cpu_state_file_path: "".to_string(),
                        //     is_last: false,
                        //     init_migration: false,
                        // };
                        
                        // DONT SERIALIZE IN FIRST ITR
                        // let data = bincode::serialize(&migration_message).unwrap();


                        let data_len = send_data.len() as u64;

                        migrate_host.write_all(&data_len.to_le_bytes()).unwrap();

                        // println!("Sending data of size: {}", send_data.len());
                        migrate_host.write_all(&send_data).unwrap();
                        // println!("itr: {}, sent data of size: {}", migration_itr, send_data.len());
                        
                    }
                    else {


                        let mut send_data : Vec<u8> = vec![];
                        let mut dpages = vec![];
                        
                        for page in dirty_pages.iter() {
                            let start_addr = vm_memory::MemoryRegionAddress((page * 4096).try_into().unwrap());
                            region.read(&mut buf[page * 4096..(page + 1) * 4096], start_addr).unwrap();
                            for i in 0..4096 {
                                send_data.push(buf.get((page * 4096 + i) as usize).unwrap().clone());
                            }
                            dpages.push(*page as u64);
                        }

                        dirty_pages_in_iters.push(dpages.len().try_into().unwrap());

                        let migration_message = MigrationMessage {
                            data_len: send_data.len(),
                            data: send_data,
                            dirty_pages_len: dpages.len(),
                            dirty_pages: dpages.clone(),
                            cpu_state_file_path: cpu_snap_path.to_string(),
                            is_last: (migration_itr == last_itr),
                            init_migration: false,
                        };


                        let data = bincode::serialize(&migration_message).unwrap();

                        // println!("Created migration message...");

                        let data_len = (data.len() as u64).to_le_bytes();

                        migrate_host.write_all(&data_len).unwrap();

                        // println!("Sending data of size: {}", data.len());
                        migrate_host.write_all(&data).unwrap();
                        // println!("itr: {}, sent data: {}", migration_itr, data.len());
                        

                        // println!("dirty_pages: {:?}", dirty_pages);
                        // print  sha hash of first dirty page

                        // assert_eq!(dpages[0] as u64, dirty_pages[0] as u64);

                        // println!("first dirty page_num: {}", dirty_pages[0]);
                        // println!("hash: {:?}", sha256::digest(&buf[dirty_pages[0] * 4096..(dirty_pages[0] + 1) * 4096]));
                        // println!("send  data first page hash: {}", first_page_in_send_data_hash);
                        // // print sha hash of first 4096 bytes of send_data

                        let mut is_stabilized = dirty_pages_in_iters.len() >= 6;
                        let diff_threshold = 100;
                        if is_stabilized {
                            let mut diff = 0;
                            for i in 0..5 {
                                diff += (dirty_pages_in_iters[dirty_pages_in_iters.len() - 1 - i] -
                                            dirty_pages_in_iters[dirty_pages_in_iters.len() - 2 - i]).abs();
                            }
                            is_stabilized = (diff) / 5 < diff_threshold;
                        }

                        let require_alteast = 3;


                        let cpu_snap_cond = !(migration_itr == last_itr)
                                         && (migration_itr > require_alteast)
                                         || (migration_itr >= require_alteast + 10)
                                         && is_stabilized;

                        if cpu_snap_cond {
                            cpu_snap_path = "./cpu_live.txt";
                            Self::save_snapshot_rpc(cpu_snap_path.to_string());
                            last_itr = migration_itr + 1;
                        }
                    }

                    thread::sleep(Duration::from_millis(500));

                    if migration_itr == last_itr {
                        break;
                    }

                    migration_itr += 1;
                }
                    
                // save memory to file (for debugging only)
                // let mut file = File::create("mem_live.txt").unwrap();
                // file.write_all(&buf).unwrap();
    
                println!("migration done");
                exit_vmm.send(true as i32).unwrap();
            });
    }


    fn save_snapshot_rpc(cpu_snap_path: String) {
        Command::new("./snapshot/target/debug/col732_project_webserver")
        .arg("resume")
        .arg(cpu_snap_path)
        .arg("./mem_snap.txt") // ignored 
        .arg("1100")
        .arg("false")
        .spawn()
        .expect("cpu snapshot failed");
    }


    // Load the kernel into guest memory.
    #[cfg(target_arch = "x86_64")]
    fn load_kernel(&mut self) -> Result<KernelLoaderResult> {
        let mut kernel_image = File::open(&self.kernel_cfg.path).map_err(Error::IO)?;
        let zero_page_addr = GuestAddress(ZEROPG_START);

        // Load the kernel into guest memory.
        let kernel_load = match Elf::load(
            &self.guest_memory,
            None,
            &mut kernel_image,
            Some(GuestAddress(self.kernel_cfg.load_addr)),
        ) {
            Ok(result) => result,
            Err(loader::Error::Elf(elf::Error::InvalidElfMagicNumber)) => BzImage::load(
                &self.guest_memory,
                None,
                &mut kernel_image,
                Some(GuestAddress(self.kernel_cfg.load_addr)),
            )
            .map_err(Error::KernelLoad)?,
            Err(e) => {
                return Err(Error::KernelLoad(e));
            }
        };

        // Generate boot parameters.
        let mut bootparams = build_bootparams(
            &self.guest_memory,
            &kernel_load,
            GuestAddress(self.kernel_cfg.load_addr),
            GuestAddress(MMIO_GAP_START),
            GuestAddress(MMIO_GAP_END),
        )
        .map_err(Error::BootParam)?;

        // Add the kernel command line to the boot parameters.
        bootparams.hdr.cmd_line_ptr = CMDLINE_START as u32;
        bootparams.hdr.cmdline_size = self.kernel_cfg.cmdline.as_str().len() as u32 + 1;

        // Load the kernel command line into guest memory.
        let mut cmdline = Cmdline::new(4096);
        cmdline
            .insert_str(self.kernel_cfg.cmdline.as_str())
            .map_err(Error::Cmdline)?;

        load_cmdline(
            &self.guest_memory,
            GuestAddress(CMDLINE_START),
            // Safe because we know the command line string doesn't contain any 0 bytes.
            &cmdline,
        )
        .map_err(Error::KernelLoad)?;

        // Write the boot parameters in the zeropage.
        LinuxBootConfigurator::write_bootparams::<GuestMemoryMmap>(
            &BootParams::new::<boot_params>(&bootparams, zero_page_addr),
            &self.guest_memory,
        )
        .map_err(Error::BootConfigure)?;

        Ok(kernel_load)
    }

    #[cfg(target_arch = "aarch64")]
    fn load_kernel(&mut self) -> Result<KernelLoaderResult> {
        let mut kernel_image = File::open(&self.kernel_cfg.path).map_err(Error::IO)?;
        linux_loader::loader::pe::PE::load(
            &self.guest_memory,
            Some(GuestAddress(self.kernel_cfg.load_addr)),
            &mut kernel_image,
            None,
        )
        .map_err(Error::KernelLoad)
    }

    // Create and add a serial console to the VMM.
    fn add_serial_console(&mut self) -> Result<()> {
        // Create the serial console.
        let interrupt_evt = EventFdTrigger::new(libc::EFD_NONBLOCK).map_err(Error::IO)?;
        let serial = Arc::new(Mutex::new(SerialWrapper(Serial::new(
            interrupt_evt.try_clone().map_err(Error::IO)?,
            stdout(),
        ))));

        // Register its interrupt fd with KVM. IRQ line 4 is typically used for serial port 1.
        // See more IRQ assignments & info: https://tldp.org/HOWTO/Serial-HOWTO-8.html
        self.vm.register_irqfd(&interrupt_evt, 4)?;

        self.kernel_cfg
            .cmdline
            .insert_str("console=ttyS0")
            .map_err(Error::Cmdline)?;

        #[cfg(target_arch = "aarch64")]
        self.kernel_cfg
            .cmdline
            .insert_str(&format!("earlycon=uart,mmio,0x{:08x}", AARCH64_MMIO_BASE))
            .map_err(Error::Cmdline)?;

        // Put it on the bus.
        // Safe to use unwrap() because the device manager is instantiated in new(), there's no
        // default implementation, and the field is private inside the VMM struct.
        #[cfg(target_arch = "x86_64")]
        {
            let range = PioRange::new(PioAddress(0x3f8), 0x8).unwrap();
            self.device_mgr
                .lock()
                .unwrap()
                .register_pio(range, serial.clone())
                .unwrap();
        }

        #[cfg(target_arch = "aarch64")]
        {
            let range = MmioRange::new(MmioAddress(AARCH64_MMIO_BASE), 0x1000).unwrap();
            self.device_mgr
                .lock()
                .unwrap()
                .register_mmio(range, serial.clone())
                .unwrap();
        }

        // Hook it to event management.
        self.event_mgr.add_subscriber(serial);

        Ok(())
    }

    // Create and add a i8042 device to the VMM.
    #[cfg(target_arch = "x86_64")]
    fn add_i8042_device(&mut self) -> Result<()> {
        let reset_evt = EventFdTrigger::new(libc::EFD_NONBLOCK).map_err(Error::IO)?;
        let i8042_device = Arc::new(Mutex::new(I8042Wrapper(I8042Device::new(
            reset_evt.try_clone().map_err(Error::IO)?,
        ))));
        self.vm.register_irqfd(&reset_evt, 1)?;
        let range = PioRange::new(PioAddress(0x060), 0x5).unwrap();

        self.device_mgr
            .lock()
            .unwrap()
            .register_pio(range, i8042_device)
            .unwrap();
        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    fn add_rtc_device(&mut self) {
        let rtc = Arc::new(Mutex::new(RtcWrapper(Rtc::new())));
        let range = MmioRange::new(MmioAddress(AARCH64_MMIO_BASE + 0x1000), 0x1000).unwrap();
        self.device_mgr
            .lock()
            .unwrap()
            .register_mmio(range, rtc)
            .unwrap();
    }

    // All methods that add a virtio device use hardcoded addresses and interrupts for now, and
    // only support a single device. We need to expand this, but it looks like a good match if we
    // can do it after figuring out how to better separate concerns and make the VMM agnostic of
    // the actual device types.
    fn add_block_device(&mut self, cfg: &BlockConfig) -> Result<()> {
        let mem = Arc::new(self.guest_memory.clone());

        let range = MmioRange::new(MmioAddress(MMIO_GAP_START), 0x1000).unwrap();
        let mmio_cfg = MmioConfig { range, gsi: 5 };

        let mut guard = self.device_mgr.lock().unwrap();

        let mut env = Env {
            mem,
            vm_fd: self.vm.vm_fd(),
            event_mgr: &mut self.event_mgr,
            mmio_mgr: guard.deref_mut(),
            mmio_cfg,
            kernel_cmdline: &mut self.kernel_cfg.cmdline,
        };

        let args = BlockArgs {
            file_path: PathBuf::from(&cfg.path),
            read_only: false,
            root_device: true,
            advertise_flush: true,
        };

        // We can also hold this somewhere if we need to keep the handle for later.
        let block = Block::new(&mut env, &args).map_err(Error::Block)?;
        self.block_devices.push(block);

        Ok(())
    }

    fn add_net_device(&mut self, cfg: &NetConfig) -> Result<()> {
        let mem = Arc::new(self.guest_memory.clone());

        let range = MmioRange::new(MmioAddress(MMIO_GAP_START + 0x2000), 0x1000).unwrap();
        let mmio_cfg = MmioConfig { range, gsi: 6 };

        let mut guard = self.device_mgr.lock().unwrap();

        let mut env = Env {
            mem,
            vm_fd: self.vm.vm_fd(),
            event_mgr: &mut self.event_mgr,
            mmio_mgr: guard.deref_mut(),
            mmio_cfg,
            kernel_cmdline: &mut self.kernel_cfg.cmdline,
        };

        let args = NetArgs {
            tap_name: cfg.tap_name.clone(),
        };

        // We can also hold this somewhere if we need to keep the handle for later.
        let net = Net::new(&mut env, &args).map_err(Error::Net)?;
        

        self.net_devices.push(net);

        Ok(())
    }

    // Helper function that computes the kernel_load_addr needed by the
    // VcpuState when creating the Vcpus.
    #[cfg(target_arch = "x86_64")]
    fn compute_kernel_load_addr(&self, kernel_load: &KernelLoaderResult) -> Result<GuestAddress> {
        // If the kernel format is bzImage, the real-mode code is offset by
        // 0x200, so that's where we have to point the rip register for the
        // first instructions to execute.
        // See https://www.kernel.org/doc/html/latest/x86/boot.html#memory-layout
        //
        // The kernel is a bzImage kernel if the protocol >= 2.00 and the 0x01
        // bit (LOAD_HIGH) in the loadflags field is set.
        let mut kernel_load_addr = self
            .guest_memory
            .check_address(kernel_load.kernel_load)
            .ok_or(Error::RipOutOfGuestMemory)?;
        if let Some(hdr) = kernel_load.setup_header {
            if hdr.version >= 0x200 && hdr.loadflags & 0x1 == 0x1 {
                // Yup, it's bzImage.
                kernel_load_addr = self
                    .guest_memory
                    .checked_offset(kernel_load_addr, 0x200)
                    .ok_or(Error::RipOutOfGuestMemory)?;
            }
        }

        Ok(kernel_load_addr)
    }

    fn check_kvm_capabilities(kvm: &Kvm) -> Result<()> {
        let capabilities = vec![Irqchip, Ioeventfd, Irqfd, UserMemory];

        // Check that all desired capabilities are supported.
        if let Some(c) = capabilities
            .iter()
            .find(|&capability| !kvm.check_extension(*capability))
        {
            Err(Error::KvmCap(*c))
        } else {
            Ok(())
        }
    }

    #[cfg(target_arch = "aarch64")]
    // TODO: move this where it makes sense from a config point of view as we add all
    // needed stuff in FDT.
    fn setup_fdt(&mut self) -> Result<()> {
        let mem_size: u64 = self.guest_memory.iter().map(|region| region.len()).sum();
        let fdt_offset = mem_size - AARCH64_FDT_MAX_SIZE - 0x10000;
        let fdt = FdtBuilder::new()
            .with_cmdline(self.kernel_cfg.cmdline.as_str())
            .with_num_vcpus(self.num_vcpus.try_into().unwrap())
            .with_mem_size(mem_size)
            .with_serial_console(0x40000000, 0x1000)
            .with_rtc(0x40001000, 0x1000)
            .create_fdt()
            .map_err(Error::SetupFdt)?;
        fdt.write_to_mem(&self.guest_memory, fdt_offset)
            .map_err(Error::SetupFdt)?;
        Ok(())
    }
}

// #[cfg(test)]
// mod tests {
//     use std::io::ErrorKind;
//     #[cfg(target_arch = "x86_64")]
//     use std::path::Path;
//     use std::path::PathBuf;

//     #[cfg(target_arch = "x86_64")]
//     use linux_loader::elf::Elf64_Ehdr;
//     #[cfg(target_arch = "x86_64")]
//     use linux_loader::loader::{self, bootparam::setup_header, elf::PvhBootCapability};
//     #[cfg(target_arch = "x86_64")]
//     use vm_memory::{
//         bytes::{ByteValued, Bytes},
//         Address, GuestAddress, GuestMemory,
//     };
//     use vmm_sys_util::{tempdir::TempDir, tempfile::TempFile};

//     use super::*;
//     use utils::resource_download::s3_download;

//     const MEM_SIZE_MIB: u32 = 1024;
//     const NUM_VCPUS: u8 = 1;

//     #[cfg(target_arch = "x86_64")]
//     fn default_bzimage_path() -> PathBuf {
//         let tags = r#"
//         {
//             "halt_after_boot": true,
//             "image_format": "bzimage"
//         }
//         "#;
//         s3_download("kernel", Some(tags)).unwrap()
//     }

//     fn default_elf_path() -> PathBuf {
//         let tags = r#"
//         {
//             "halt_after_boot": true,
//             "image_format": "elf"
//         }
//         "#;
//         s3_download("kernel", Some(tags)).unwrap()
//     }

//     #[cfg(target_arch = "aarch64")]
//     fn default_pe_path() -> PathBuf {
//         let tags = r#"
//         {
//             "halt_after_boot": true,
//             "image_format": "pe"
//         }
//         "#;
//         s3_download("kernel", Some(tags)).unwrap()
//     }

//     fn default_vmm_config() -> VMMConfig {
//         VMMConfig {
//             kernel_config: KernelConfig {
//                 #[cfg(target_arch = "x86_64")]
//                 path: default_elf_path(),
//                 #[cfg(target_arch = "aarch64")]
//                 path: default_pe_path(),
//                 load_addr: DEFAULT_KERNEL_LOAD_ADDR,
//                 cmdline: KernelConfig::default_cmdline(),
//             },
//             memory_config: MemoryConfig {
//                 size_mib: MEM_SIZE_MIB,
//             },
//             vcpu_config: VcpuConfig { num: NUM_VCPUS },
//             block_config: None,
//             net_config: None,
//         }
//     }

//     fn default_exit_handler() -> WrappedExitHandler {
//         WrappedExitHandler(Arc::new(Mutex::new(VmmExitHandler {
//             keep_running: AtomicBool::default(),
//             exit_event: EventFd::new(libc::EFD_NONBLOCK).unwrap(),
//         })))
//     }

//     // Returns a VMM which only has the memory configured. The purpose of the mock VMM
//     // is to give a finer grained control to test individual private functions in the VMM.
//     fn mock_vmm(vmm_config: VMMConfig) -> Vmm {
//         let kvm = Kvm::new().unwrap();
//         let guest_memory = Vmm::create_guest_memory(&vmm_config.memory_config).unwrap();

//         // Create the KvmVm.
//         let vm_config = VmConfig::new(&kvm, vmm_config.vcpu_config.num).unwrap();

//         let device_mgr = Arc::new(Mutex::new(IoManager::new()));
//         let exit_handler = default_exit_handler();
//         let vm = KvmVm::new(
//             &kvm,
//             vm_config,
//             &guest_memory,
//             exit_handler.clone(),
//             device_mgr.clone(),
//         )
//         .unwrap();

//         Vmm {
//             vm,
//             guest_memory,
//             device_mgr,
//             event_mgr: EventManager::new().unwrap(),
//             kernel_cfg: vmm_config.kernel_config,
//             exit_handler,
//             block_devices: Vec::new(),
//             net_devices: Vec::new(),
//             #[cfg(target_arch = "aarch64")]
//             num_vcpus: vmm_config.vcpu_config.num as u64,
//         }
//     }

//     // Return the address where an ELF file should be loaded, as specified in its header.
//     #[cfg(target_arch = "x86_64")]
//     fn elf_load_addr(elf_path: &Path) -> GuestAddress {
//         let mut elf_file = File::open(elf_path).unwrap();
//         let mut ehdr = Elf64_Ehdr::default();
//         ehdr.as_bytes()
//             .read_from(0, &mut elf_file, std::mem::size_of::<Elf64_Ehdr>())
//             .unwrap();
//         GuestAddress(ehdr.e_entry)
//     }

//     #[test]
//     #[cfg(target_arch = "x86_64")]
//     fn test_compute_kernel_load_addr() {
//         let vmm_config = default_vmm_config();
//         let vmm = mock_vmm(vmm_config);

//         // ELF (vmlinux) kernel scenario: happy case
//         let mut kern_load = KernelLoaderResult {
//             kernel_load: GuestAddress(DEFAULT_HIGH_RAM_START), // 1 MiB.
//             kernel_end: 0,                                     // doesn't matter.
//             setup_header: None,
//             pvh_boot_cap: PvhBootCapability::PvhEntryNotPresent,
//         };
//         let actual_kernel_load_addr = vmm.compute_kernel_load_addr(&kern_load).unwrap();
//         let expected_load_addr = kern_load.kernel_load;
//         assert_eq!(actual_kernel_load_addr, expected_load_addr);

//         kern_load.kernel_load = GuestAddress(vmm.guest_memory.last_addr().raw_value() + 1);
//         assert!(matches!(
//             vmm.compute_kernel_load_addr(&kern_load),
//             Err(Error::RipOutOfGuestMemory)
//         ));

//         // bzImage kernel scenario: happy case
//         // The difference is that kernel_load.setup_header is no longer None, because we found one
//         // while parsing the bzImage file.
//         kern_load.kernel_load = GuestAddress(0x0100_0000); // 1 MiB.
//         kern_load.setup_header = Some(setup_header {
//             version: 0x0200, // 0x200 (v2.00) is the minimum.
//             loadflags: 1,
//             ..Default::default()
//         });
//         let expected_load_addr = kern_load.kernel_load.unchecked_add(0x200);
//         let actual_kernel_load_addr = vmm.compute_kernel_load_addr(&kern_load).unwrap();
//         assert_eq!(expected_load_addr, actual_kernel_load_addr);

//         // bzImage kernel scenario: error case: kernel_load + 0x200 (512 - size of bzImage header)
//         // falls out of guest memory
//         kern_load.kernel_load = GuestAddress(vmm.guest_memory.last_addr().raw_value() - 511);
//         assert!(matches!(
//             vmm.compute_kernel_load_addr(&kern_load),
//             Err(Error::RipOutOfGuestMemory)
//         ));
//     }

//     #[test]
//     #[cfg(target_arch = "x86_64")]
//     fn test_load_kernel() {
//         // Test Case: load a valid elf.
//         let mut vmm_config = default_vmm_config();
//         vmm_config.kernel_config.path = default_elf_path();
//         // ELF files start with a header that tells us where they want to be loaded.
//         let kernel_load = elf_load_addr(&vmm_config.kernel_config.path);
//         let mut vmm = mock_vmm(vmm_config);
//         let kernel_load_result = vmm.load_kernel().unwrap();
//         assert_eq!(kernel_load_result.kernel_load, kernel_load);
//         assert!(kernel_load_result.setup_header.is_none());

//         // Test case: load a valid bzImage.
//         let mut vmm_config = default_vmm_config();
//         vmm_config.kernel_config.path = default_bzimage_path();
//         let mut vmm = mock_vmm(vmm_config);
//         let kernel_load_result = vmm.load_kernel().unwrap();
//         assert_eq!(
//             kernel_load_result.kernel_load,
//             GuestAddress(DEFAULT_HIGH_RAM_START)
//         );
//         assert!(kernel_load_result.setup_header.is_some());
//     }

//     #[test]
//     fn test_load_kernel_errors() {
//         // Test case: kernel file does not exist.
//         let mut vmm_config = default_vmm_config();
//         vmm_config.kernel_config.path = PathBuf::from(TempFile::new().unwrap().as_path());
//         let mut vmm = mock_vmm(vmm_config);
//         assert!(
//             matches!(vmm.load_kernel().unwrap_err(), Error::IO(e) if e.kind() == ErrorKind::NotFound)
//         );

//         // Test case: kernel image is invalid.
//         let mut vmm_config = default_vmm_config();
//         let temp_file = TempFile::new().unwrap();
//         vmm_config.kernel_config.path = PathBuf::from(temp_file.as_path());
//         let mut vmm = mock_vmm(vmm_config);

//         let err = vmm.load_kernel().unwrap_err();
//         #[cfg(target_arch = "x86_64")]
//         assert!(matches!(
//             err,
//             Error::KernelLoad(loader::Error::Bzimage(
//                 loader::bzimage::Error::InvalidBzImage
//             ))
//         ));
//         #[cfg(target_arch = "aarch64")]
//         assert!(matches!(
//             err,
//             Error::KernelLoad(loader::Error::Pe(
//                 loader::pe::Error::InvalidImageMagicNumber
//             ))
//         ));

//         // Test case: kernel path doesn't point to a file.
//         let mut vmm_config = default_vmm_config();
//         let temp_dir = TempDir::new().unwrap();
//         vmm_config.kernel_config.path = PathBuf::from(temp_dir.as_path());
//         let mut vmm = mock_vmm(vmm_config);
//         let err = vmm.load_kernel().unwrap_err();

//         #[cfg(target_arch = "x86_64")]
//         assert!(matches!(
//             err,
//             Error::KernelLoad(loader::Error::Elf(loader::elf::Error::ReadElfHeader))
//         ));
//         #[cfg(target_arch = "aarch64")]
//         assert!(matches!(
//             err,
//             Error::KernelLoad(loader::Error::Pe(loader::pe::Error::ReadImageHeader))
//         ));
//     }

//     #[test]
//     #[cfg(target_arch = "aarch64")]
//     fn test_load_kernel() {
//         // Test case: Loading the default & valid image is ok.
//         let vmm_config = default_vmm_config();
//         let mut vmm = mock_vmm(vmm_config);
//         assert!(vmm.load_kernel().is_ok());
//     }

//     #[test]
//     fn test_cmdline_updates() {
//         let mut vmm_config = default_vmm_config();
//         vmm_config.kernel_config.path = default_elf_path();
//         let mut vmm = mock_vmm(vmm_config);
//         assert_eq!(vmm.kernel_cfg.cmdline.as_str(), DEFAULT_KERNEL_CMDLINE);

//         vmm.add_serial_console().unwrap();
//         #[cfg(target_arch = "x86_64")]
//         assert!(vmm.kernel_cfg.cmdline.as_str().contains("console=ttyS0"));
//         #[cfg(target_arch = "aarch64")]
//         assert!(vmm
//             .kernel_cfg
//             .cmdline
//             .as_str()
//             .contains("earlycon=uart,mmio"));
//     }

//     #[test]
//     #[cfg(target_arch = "x86_64")]
//     fn test_create_guest_memory() {
//         // Guest memory ends exactly at the MMIO gap: should succeed (last addressable value is
//         // MMIO_GAP_START - 1). There should be 1 memory region.
//         let mut mem_cfg = MemoryConfig {
//             size_mib: (MMIO_GAP_START >> 20) as u32,
//         };
//         let guest_mem = Vmm::create_guest_memory(&mem_cfg).unwrap();
//         assert_eq!(guest_mem.num_regions(), 1);
//         assert_eq!(guest_mem.last_addr(), GuestAddress(MMIO_GAP_START - 1));

//         // Guest memory ends exactly past the MMIO gap: not possible because it's specified in MiB.
//         // But it can end 1 MiB within the MMIO gap. Should succeed.
//         // There will be 2 regions, the 2nd ending at `size_mib << 20 + MMIO_GAP_SIZE`.
//         mem_cfg.size_mib = (MMIO_GAP_START >> 20) as u32 + 1;
//         let guest_mem = Vmm::create_guest_memory(&mem_cfg).unwrap();
//         assert_eq!(guest_mem.num_regions(), 2);
//         assert_eq!(
//             guest_mem.last_addr(),
//             GuestAddress(MMIO_GAP_START + MMIO_GAP_SIZE + (1 << 20) - 1)
//         );

//         // Guest memory ends exactly at the MMIO gap end: should succeed. There will be 2 regions,
//         // the 2nd ending at `size_mib << 20 + MMIO_GAP_SIZE`.
//         mem_cfg.size_mib = ((MMIO_GAP_START + MMIO_GAP_SIZE) >> 20) as u32;
//         let guest_mem = Vmm::create_guest_memory(&mem_cfg).unwrap();
//         assert_eq!(guest_mem.num_regions(), 2);
//         assert_eq!(
//             guest_mem.last_addr(),
//             GuestAddress(MMIO_GAP_START + 2 * MMIO_GAP_SIZE - 1)
//         );

//         // Guest memory ends 1 MiB past the MMIO gap end: should succeed. There will be 2 regions,
//         // the 2nd ending at `size_mib << 20 + MMIO_GAP_SIZE`.
//         mem_cfg.size_mib = ((MMIO_GAP_START + MMIO_GAP_SIZE) >> 20) as u32 + 1;
//         let guest_mem = Vmm::create_guest_memory(&mem_cfg).unwrap();
//         assert_eq!(guest_mem.num_regions(), 2);
//         assert_eq!(
//             guest_mem.last_addr(),
//             GuestAddress(MMIO_GAP_START + 2 * MMIO_GAP_SIZE + (1 << 20) - 1)
//         );

//         // Guest memory size is 0: should fail, rejected by vm-memory with EINVAL.
//         mem_cfg.size_mib = 0u32;
//         assert!(matches!(
//             Vmm::create_guest_memory(&mem_cfg),
//             Err(Error::Memory(MemoryError::VmMemory(vm_memory::Error::MmapRegion(vm_memory::mmap::MmapRegionError::Mmap(e)))))
//             if e.kind() == ErrorKind::InvalidInput
//         ));
//     }

//     #[test]
//     fn test_create_vcpus() {
//         // The scopes force the created vCPUs to unmap their kernel memory at the end.
//         let mut vmm_config = default_vmm_config();
//         vmm_config.vcpu_config = VcpuConfig { num: 0 };

//         // Creating 0 vCPUs throws an error.
//         {
//             assert!(matches!(
//                 Vmm::try_from(vmm_config.clone()),
//                 Err(Error::Vm(vm::Error::CreateVmConfig(
//                     vm_vcpu::vcpu::Error::VcpuNumber(0)
//                 )))
//             ));
//         }

//         // Creating one works.
//         vmm_config.vcpu_config = VcpuConfig { num: 1 };
//         {
//             assert!(Vmm::try_from(vmm_config.clone()).is_ok());
//         }

//         // Creating 254 also works (that's the maximum number on x86 when using MP Table).
//         vmm_config.vcpu_config = VcpuConfig { num: 254 };
//         Vmm::try_from(vmm_config).unwrap();
//     }

//     #[test]
//     #[cfg(target_arch = "x86_64")]
//     // FIXME: We cannot run this on aarch64 because we do not have an image that just runs and
//     // FIXME-continued: halts afterwards. Once we have this, we need to update `default_vmm_config`
//     // FIXME-continued: and have a default PE image on aarch64.
//     fn test_add_block() {
//         let vmm_config = default_vmm_config();
//         let mut vmm = mock_vmm(vmm_config);

//         let tempfile = TempFile::new().unwrap();
//         let block_config = BlockConfig {
//             path: tempfile.as_path().to_path_buf(),
//         };

//         assert!(vmm.add_block_device(&block_config).is_ok());
//         assert_eq!(vmm.block_devices.len(), 1);
//         assert!(vmm.kernel_cfg.cmdline.as_str().contains("virtio"));

//         let invalid_block_config = BlockConfig {
//             // Let's create the tempfile directly here so that it gets out of scope immediately
//             // and delete the underlying file.
//             path: TempFile::new().unwrap().as_path().to_path_buf(),
//         };

//         let err = vmm.add_block_device(&invalid_block_config).unwrap_err();
//         assert!(
//             matches!(err, Error::Block(block::Error::OpenFile(io_err)) if io_err.kind() == ErrorKind::NotFound)
//         );

//         // The current implementation of the VMM does not allow more than one block device
//         // as we are hard coding the `MmioConfig`.
//         // This currently fails because a device is already registered with the provided
//         // MMIO range.
//         assert!(vmm.add_block_device(&block_config).is_err());
//     }

//     #[test]
//     #[cfg(target_arch = "x86_64")]
//     // FIXME: We cannot run this on aarch64 because we do not have an image that just runs and
//     // FIXME-continued: halts afterwards. Once we have this, we need to update `default_vmm_config`
//     // FIXME-continued: and have a default PE image on aarch64.
//     fn test_add_net() {
//         let vmm_config = default_vmm_config();
//         let mut vmm = mock_vmm(vmm_config);

//         // The device only attempts to open the tap interface during activation, so we can
//         // specify any name here for now.
//         let cfg = NetConfig {
//             tap_name: "imaginary_tap".to_owned(),
//         };

//         {
//             assert!(vmm.add_net_device(&cfg).is_ok());
//             assert_eq!(vmm.net_devices.len(), 1);
//             assert!(vmm.kernel_cfg.cmdline.as_str().contains("virtio"));
//         }

//         {
//             // The current implementation of the VMM does not allow more than one net device
//             // as we are hard coding the `MmioConfig`.
//             // This currently fails because a device is already registered with the provided
//             // MMIO range.
//             assert!(vmm.add_net_device(&cfg).is_err());
//         }
//     }

//     #[test]
//     #[cfg(target_arch = "aarch64")]
//     fn test_setup_fdt() {
//         let vmm_config = default_vmm_config();
//         let mut vmm = mock_vmm(vmm_config);

//         {
//             let result = vmm.setup_fdt();
//             assert!(result.is_ok());
//         }

//         {
//             let mem_size: u64 = vmm.guest_memory.iter().map(|region| region.len()).sum();
//             let fdt_offset = mem_size + AARCH64_FDT_MAX_SIZE;
//             let fdt = FdtBuilder::new()
//                 .with_cmdline(vmm.kernel_cfg.cmdline.as_str())
//                 .with_num_vcpus(vmm.num_vcpus.try_into().unwrap())
//                 .with_mem_size(mem_size)
//                 .with_serial_console(0x40000000, 0x1000)
//                 .with_rtc(0x40001000, 0x1000)
//                 .create_fdt()
//                 .unwrap();
//             assert!(fdt.write_to_mem(&vmm.guest_memory, fdt_offset).is_err());
//         }
//     }
// }
