//! An example utilizing the `EmbassyWifiMatterStack` struct.
//!
//! As the name suggests, this Matter stack assembly uses Wifi as the main transport,
//! and thus BLE for commissioning.
//!
//! If you want to use Ethernet, utilize `EmbassyEthMatterStack` instead.
//! If you want to use non-concurrent commissioning, utilize `EmbassyWifiNCMatterStack` instead
//! (Note: Alexa does not work (yet) with non-concurrent commissioning.)
//!
//! The example implements a fictitious Light device (an On-Off Matter cluster).
#![no_std]
#![no_main]

use core::mem::MaybeUninit;
use core::pin::pin;
use core::ptr::addr_of_mut;

use bt_hci::controller::ExternalController;

use cyw43_pio::PioSpi;

use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_time::{Duration, Timer};

use embedded_alloc::LlffHeap;

use log::info;

use rs_matter_embassy::rand::rp::rp_rand;
use rs_matter_embassy::epoch::epoch;
use rs_matter_embassy::matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter_embassy::matter::data_model::cluster_on_off;
use rs_matter_embassy::matter::data_model::device_types::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter_embassy::matter::data_model::objects::{Dataver, Endpoint, HandlerCompat, Node};
use rs_matter_embassy::matter::data_model::system_model::descriptor;
use rs_matter_embassy::matter::utils::init::InitMaybeUninit;
use rs_matter_embassy::matter::utils::select::Coalesce;
use rs_matter_embassy::stack::persist::DummyPersist;
use rs_matter_embassy::stack::test_device::{
    TEST_BASIC_COMM_DATA, TEST_DEV_ATT, TEST_PID, TEST_VID,
};
use rs_matter_embassy::stack::MdnsType;
use rs_matter_embassy::wireless::{EmbassyBle, PreexistingBleController};
use rs_matter_embassy::wireless::wifi::rp::Cyw43WifiController;
use rs_matter_embassy::wireless::wifi::{EmbassyWifi, EmbassyWifiMatterStack, PreexistingWifiDriver};

macro_rules! mk_static {
    ($t:ty) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit();
        x
    }};
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});

#[global_allocator]
static HEAP: LlffHeap = LlffHeap::empty();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // `rs-matter` uses the `x509` crate which (still) needs a few kilos of heap space
    {
        const HEAP_SIZE: usize = 8192;
        
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
    }

    info!("Starting...");

    // == Step 1: ==
    // Necessary `esp-hal` and `esp-wifi` initialization boilerplate

    let p = embassy_rp::init(Default::default());

    #[cfg(feature = "skip-cyw43-firmware")]
    let (fw, clm, btfw) = (&[], &[], &[]);

    #[cfg(not(feature = "skip-cyw43-firmware"))]
    let (fw, clm, btfw) = {
        // IMPORTANT
        //
        // Download and make sure these files from https://github.com/embassy-rs/embassy/tree/main/cyw43-firmware
        // are available in `./examples/rp-pico-w`. (should be automatic)
        //
        // IMPORTANT
        let fw = include_bytes!("../../cyw43-firmware/43439A0.bin");
        let clm = include_bytes!("../../cyw43-firmware/43439A0_clm.bin");
        let btfw = include_bytes!("../../cyw43-firmware/43439A0_btfw.bin");
        (fw, clm, btfw)
    };

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        cyw43_pio::DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        p.DMA_CH0,
    );

    let state = mk_static!(cyw43::State, cyw43::State::new());
    let (net_device, bt_device, mut control, runner) = cyw43::new_with_bluetooth(state, pwr, spi, fw, btfw).await;
    spawner.spawn(cyw43_task(runner)).unwrap();
    control.init(clm).await;

    let controller: ExternalController<_, 10> = ExternalController::new(bt_device);

     // == Step 2: ==
    // Statically allocate the Matter stack.
    // For MCUs, it is best to allocate it statically, so as to avoid program stack blowups (its memory footprint is ~ 35 to 50KB).
    // It is also (currently) a mandatory requirement when the wireless stack variation is used.
    let stack = mk_static!(EmbassyWifiMatterStack<()>).init_with(EmbassyWifiMatterStack::init(
        &BasicInfoConfig {
            vid: TEST_VID,
            pid: TEST_PID,
            hw_ver: 2,
            sw_ver: 1,
            sw_ver_str: "1",
            serial_no: "aabbccdd",
            device_name: "MyLight",
            product_name: "ACME Light",
            vendor_name: "ACME",
        },
        TEST_BASIC_COMM_DATA,
        &TEST_DEV_ATT,
        MdnsType::Builtin,
        epoch,
        rp_rand,
    ));

    // == Step 3: ==
    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::data_model::AsyncHandler`
    let on_off = cluster_on_off::OnOffCluster::new(Dataver::new_rand(stack.matter().rand()));

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = stack
        .root_handler()
        // Our on-off cluster, on Endpoint 1
        .chain(
            LIGHT_ENDPOINT_ID,
            cluster_on_off::ID,
            HandlerCompat(&on_off),
        )
        // Each Endpoint needs a Descriptor cluster too
        // Just use the one that `rs-matter` provides out of the box
        .chain(
            LIGHT_ENDPOINT_ID,
            descriptor::ID,
            HandlerCompat(descriptor::DescriptorCluster::new(Dataver::new_rand(
                stack.matter().rand(),
            ))),
        );

    // == Step 4: ==
    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    //
    // This step can be repeated in that the stack can be stopped and started multiple times, as needed.
    let mut matter = pin!(stack.run(
        // The Matter stack needs to instantiate an `embassy-net` `Driver` and `Controller`
        EmbassyWifi::new(PreexistingWifiDriver::new(net_device, Cyw43WifiController::new(control)), stack),
        // The Matter stack needs BLE
        EmbassyBle::new(PreexistingBleController::new(controller), stack),
        // The Matter stack needs a persister to store its state
        // `EmbassyPersist`+`EmbassyKvBlobStore` saves to a user-supplied NOR Flash region
        // However, for this demo and for simplicity, we use a dummy persister that does nothing
        DummyPersist,
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // No user future to run
        core::future::pending(),
    ));

    // Just for demoing purposes:
    //
    // Run a sample loop that simulates state changes triggered by the HAL
    // Changes will be properly communicated to the Matter controllers
    // (i.e. Google Home, Alexa) and other Matter devices thanks to subscriptions
    let mut device = pin!(async {
        loop {
            // Simulate user toggling the light with a physical switch every 5 seconds
            Timer::after(Duration::from_secs(5)).await;

            // Toggle
            on_off.set(!on_off.get());

            // Let the Matter stack know that we have changed
            // the state of our Light device
            stack.notify_changed();

            info!("Light toggled");
        }
    });

    // Schedule the Matter run & the device loop together
    select(&mut matter, &mut device).coalesce().await.unwrap();
}

#[embassy_executor::task]
async fn cyw43_task(runner: cyw43::Runner<'static, Output<'static>, PioSpi<'static, PIO0, 0, DMA_CH0>>) -> ! {
    runner.run().await
}

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EmbassyWifiMatterStack::<()>::root_metadata(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_types: &[DEV_TYPE_ON_OFF_LIGHT],
            clusters: &[descriptor::CLUSTER, cluster_on_off::CLUSTER],
        },
    ],
};
