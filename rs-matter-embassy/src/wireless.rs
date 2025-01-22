//! Wireless: Type aliases and state structs for an Embassy Matter stack running over a wireless network (Wifi or Thread) and BLE.

use core::mem::MaybeUninit;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use rs_matter::tlv::{FromTLV, ToTLV};
use rs_matter_stack::matter::error::Error;
use rs_matter_stack::matter::utils::init::{init, Init};
use rs_matter_stack::matter::utils::rand::Rand;
use rs_matter_stack::matter::utils::sync::IfMutex;
use rs_matter_stack::network::{Embedding, Network};
use rs_matter_stack::persist::KvBlobBuf;
use rs_matter_stack::wireless::traits::{Ble, BleTask, WirelessConfig, WirelessData};
use rs_matter_stack::{MatterStack, WirelessBle};
use trouble_host::Controller;

use crate::ble::{BleControllerProvider, TroubleBtpGattContext, TroubleBtpGattPeripheral};
use crate::nal::{MatterStackResources, MatterUdpBuffers};

pub use wifi::*;

/// A type alias for an Embassy Matter stack running over a wireless network (Wifi or Thread) and BLE.
pub type EmbassyWirelessMatterStack<'a, T, E = ()> = MatterStack<'a, EmbassyWirelessBle<T, E>>;

/// A type alias for an Embassy implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over either WiFi or Thread when operating.
pub type EmbassyWirelessBle<T, E = ()> =
    WirelessBle<CriticalSectionRawMutex, T, KvBlobBuf<EmbassyGatt<E>>>;

/// An embedding of the Trouble Gatt peripheral context for the `WirelessBle` network type from `rs-matter-stack`.
///
/// Allows the memory of this context to be statically allocated and cost-initialized.
///
/// Usage:
/// ```no_run
/// MatterStack<WirelessBle<CriticalSectionRawMutex, Wifi, KvBlobBuf<EmbassyGatt<C, E>>>>::new(...);
/// ```
///
/// ... where `E` can be a next-level, user-supplied embedding or just `()` if the user does not need to embed anything.
pub struct EmbassyGatt<E = ()> {
    btp_gatt_context: TroubleBtpGattContext<CriticalSectionRawMutex>,
    enet_context: EmbassyNetContext,
    embedding: E,
}

impl<E> EmbassyGatt<E>
where
    E: Embedding,
{
    /// Creates a new instance of the `EspGatt` embedding.
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    const fn new() -> Self {
        Self {
            btp_gatt_context: TroubleBtpGattContext::new(),
            enet_context: EmbassyNetContext::new(),
            embedding: E::INIT,
        }
    }

    /// Return an in-place initializer for the `EspGatt` embedding.
    fn init() -> impl Init<Self> {
        init!(Self {
            btp_gatt_context <- TroubleBtpGattContext::init(),
            enet_context <- EmbassyNetContext::init(),
            embedding <- E::init(),
        })
    }

    /// Return a reference to the Bluedroid Gatt peripheral context.
    pub fn ble_context(&self) -> &TroubleBtpGattContext<CriticalSectionRawMutex> {
        &self.btp_gatt_context
    }

    pub fn enet_context(&self) -> &EmbassyNetContext {
        &self.enet_context
    }

    /// Return a reference to the embedding.
    pub fn embedding(&self) -> &E {
        &self.embedding
    }
}

impl<E> Embedding for EmbassyGatt<E>
where
    E: Embedding,
{
    const INIT: Self = Self::new();

    fn init() -> impl Init<Self> {
        EmbassyGatt::init()
    }
}

/// A context (storage) for the network layer of the Matter stack.
pub struct EmbassyNetContext {
    buffers: MatterUdpBuffers,
    resources: IfMutex<CriticalSectionRawMutex, MatterStackResources>,
}

impl EmbassyNetContext {
    /// Create a new instance of the `EmbassyNetContext` type.
    pub const fn new() -> Self {
        Self {
            buffers: MatterUdpBuffers::new(),
            resources: IfMutex::new(MatterStackResources::new()),
        }
    }

    /// Return an in-place initializer for the `EmbassyNetContext` type.
    pub fn init() -> impl Init<Self> {
        init!(Self {
            // TODO: Implement init constructor for `UdpBuffers`
            buffers: MatterUdpBuffers::new(),
            // Note: below will break if `HostResources` stops being a bunch of `MaybeUninit`s
            resources <- IfMutex::init(unsafe { MaybeUninit::<MatterStackResources>::uninit().assume_init() }),
        })
    }
}

impl Default for EmbassyNetContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A `Ble` trait implementation for `trouble`'s BLE stack
pub struct EmbassyBle<'a, T> {
    provider: T,
    rand: Rand,
    context: &'a TroubleBtpGattContext<CriticalSectionRawMutex>,
}

impl<'a, T> EmbassyBle<'a, T>
where
    T: BleControllerProvider,
{
    /// Create a new instance of the `EmbassyBle` type.
    pub fn new<E>(provider: T, stack: &'a EmbassyWifiMatterStack<'a, E>) -> Self
    where
        E: Embedding + 'static,
    {
        Self::wrap(
            provider,
            stack.matter().rand(),
            stack.network().embedding().embedding().ble_context(),
        )
    }

    /// Wrap the `EmbassyBle` type around a BLE controller provider and a trouble BTP GATT context.
    pub const fn wrap(
        provider: T,
        rand: Rand,
        context: &'a TroubleBtpGattContext<CriticalSectionRawMutex>,
    ) -> Self {
        Self {
            provider,
            rand,
            context,
        }
    }
}

impl<T> Ble for EmbassyBle<'_, T>
where
    T: BleControllerProvider,
{
    async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
    where
        A: BleTask,
    {
        let controller = self.provider.provide().await;

        let peripheral = TroubleBtpGattPeripheral::new(controller, self.rand, self.context);

        task.run(&peripheral).await
    }
}

impl<'a, C> TroubleBtpGattPeripheral<'a, CriticalSectionRawMutex, C>
where 
    C: Controller,
{
    pub fn new_for_stack<T, E>(controller: C, stack: &'a crate::wireless::EmbassyWirelessMatterStack<T, E>) -> Self 
    where 
        T: WirelessConfig,
        <T::Data as WirelessData>::NetworkCredentials: Clone + for<'t> FromTLV<'t> + ToTLV,
        E: Embedding + 'static,
    {
        Self::new(controller, stack.matter().rand(), stack.network().embedding().embedding().ble_context())
    }
}

// Wifi: Type aliases and state structs for an Embassy Matter stack running over a Wifi network and BLE.
mod wifi {
    use core::pin::pin;

    use edge_nal_embassy::Udp;
    use embassy_futures::select::select;

    use rs_matter_stack::matter::error::Error;
    use rs_matter_stack::matter::utils::rand::Rand;
    use rs_matter_stack::matter::utils::select::Coalesce;
    use rs_matter_stack::network::{Embedding, Network};
    use rs_matter_stack::wireless::traits::{
        Controller, Wifi, WifiData, Wireless, WirelessTask, NC,
    };

    use crate::nal::create_net_stack;
    use crate::netif::EmbassyNetif;

    use super::{EmbassyNetContext, EmbassyWirelessMatterStack};

    /// A type alias for an Embassy Matter stack running over Wifi (and BLE, during commissioning).
    pub type EmbassyWifiMatterStack<'a, E> = EmbassyWirelessMatterStack<'a, Wifi, E>;

    /// A type alias for an Embassy Matter stack running over Wifi (and BLE, during commissioning).
    ///
    /// Unlike `EmbassyWifiMatterStack`, this type alias runs the commissioning in a non-concurrent mode,
    /// where the device runs either BLE or Wifi, but not both at the same time.
    ///
    /// This is useful to save memory by only having one of the stacks active at any point in time.
    ///
    /// Note that Alexa does not (yet) work with non-concurrent commissioning.
    pub type EmbassyWifiNCMatterStack<'a, E> = EmbassyWirelessMatterStack<'a, Wifi<NC>, E>;

    /// A companion trait of `EmbassyWifi` for providing a Wifi driver and controller.
    pub trait WifiDriverProvider {
        type Driver<'a>: embassy_net::driver::Driver
        where
            Self: 'a;
        type Controller<'a>: Controller<Data = WifiData>
        where
            Self: 'a;

        /// Provide a Wifi driver and controller by creating these when the Matter stack needs them
        async fn provide(&mut self) -> (Self::Driver<'_>, Self::Controller<'_>);
    }

    impl<T> WifiDriverProvider for &mut T
    where
        T: WifiDriverProvider,
    {
        type Driver<'a>
            = T::Driver<'a>
        where
            Self: 'a;
        type Controller<'a>
            = T::Controller<'a>
        where
            Self: 'a;

        async fn provide(&mut self) -> (Self::Driver<'_>, Self::Controller<'_>) {
            (*self).provide().await
        }
    }

    pub struct PreexistingWifi<D, C>(pub D, pub C);

    impl<D, C> WifiDriverProvider for PreexistingWifi<D, C>
    where
        D: embassy_net::driver::Driver,
        C: Controller<Data = WifiData>,
    {
        type Driver<'a> = &'a mut D where Self: 'a;
        type Controller<'a> = &'a mut C where Self: 'a;

        async fn provide(&mut self) -> (Self::Driver<'_>, Self::Controller<'_>) {
            (&mut self.0, &mut self.1)
        }
    }

    /// A `Wireless` trait implementation for `embassy-net`'s Wifi stack.
    pub struct EmbassyWifi<'a, T> {
        provider: T,
        context: &'a EmbassyNetContext,
        rand: Rand,
    }

    impl<'a, T> EmbassyWifi<'a, T>
    where
        T: WifiDriverProvider,
    {
        /// Create a new instance of the `EmbassyWifi` type.
        pub fn new<E>(provider: T, stack: &'a EmbassyWifiMatterStack<'a, E>) -> Self
        where
            E: Embedding + 'static,
        {
            Self::wrap(
                provider,
                stack.network().embedding().embedding().enet_context(),
                stack.matter().rand(),
            )
        }

        /// Wrap the `EmbassyWifi` type around a Wifi driver provider and a network context.
        pub const fn wrap(provider: T, context: &'a EmbassyNetContext, rand: Rand) -> Self {
            Self {
                provider,
                context,
                rand,
            }
        }
    }

    impl<T> Wireless for EmbassyWifi<'_, T>
    where
        T: WifiDriverProvider,
    {
        type Data = WifiData;

        async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
        where
            A: WirelessTask<Data = Self::Data>,
        {
            let (driver, controller) = self.provider.provide().await;

            let mut resources = self.context.resources.lock().await;
            let resources = &mut *resources;
            let buffers = &self.context.buffers;

            let mut seed = [0; core::mem::size_of::<u64>()];
            (self.rand)(&mut seed);

            let (stack, mut runner) = create_net_stack(driver, u64::from_le_bytes(seed), resources);

            let netif = EmbassyNetif::new(stack);
            let udp = Udp::new(stack, buffers);

            let mut main = pin!(task.run(netif, udp, controller));
            let mut run = pin!(async {
                runner.run().await;
                #[allow(unreachable_code)]
                Ok(())
            });

            select(&mut main, &mut run).coalesce().await
        }
    }

    #[cfg(feature = "rp")]
    pub mod rp {
        use cyw43::Control;

        use rs_matter::error::Error;
        use rs_matter_stack::wireless::traits::{Controller, NetworkCredentials, WifiData, WifiSsid, WirelessData};

        pub struct Cyw43WifiController<'a>(Control<'a>, Option<WifiSsid>);

        impl<'a> Cyw43WifiController<'a> {
            /// Create a new instance of the `Esp32Controller` type.
            ///
            /// # Arguments
            /// - `controller` - The `esp-wifi` Wifi controller instance.
            pub const fn new(controller: Control<'a>) -> Self {
                Self(controller, None)
            }
        }

        impl Controller for Cyw43WifiController<'_> {
            type Data = WifiData;

            async fn scan<F>(
                &mut self,
                network_id: Option<
                    &<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId,
                >,
                mut callback: F,
            ) -> Result<(), Error>
            where
                F: FnMut(Option<&<Self::Data as WirelessData>::ScanResult>) -> Result<(), Error>,
            {
                // if !self.0.is_started().map_err(to_err)? {
                //     self.0.start_async().await.map_err(to_err)?;
                // }

                // let mut scan_config = ScanConfig::default();
                // if let Some(network_id) = network_id {
                //     scan_config.ssid = Some(network_id.0.as_str());
                // }

                // let (aps, _) = self
                //     .0
                //     .scan_with_config_async::<MAX_NETWORKS>(scan_config)
                //     .await
                //     .map_err(to_err)?;

                // for ap in aps {
                //     callback(Some(&WifiScanResult {
                //         ssid: WifiSsid(ap.ssid),
                //         bssid: OctetsOwned {
                //             vec: Vec::from_slice(&ap.bssid).unwrap(),
                //         },
                //         channel: ap.channel as _,
                //         rssi: Some(ap.signal_strength),
                //         band: None,
                //         security: match ap.auth_method {
                //             Some(AuthMethod::None) => WiFiSecurity::Unencrypted,
                //             Some(AuthMethod::WEP) => WiFiSecurity::Wep,
                //             Some(AuthMethod::WPA) => WiFiSecurity::WpaPersonal,
                //             Some(AuthMethod::WPA3Personal) => WiFiSecurity::Wpa3Personal,
                //             _ => WiFiSecurity::Wpa2Personal,
                //         },
                //     }))?;
                // }

                // callback(None)?;

                Ok(())
            }

            async fn connect(
                &mut self,
                creds: &<Self::Data as WirelessData>::NetworkCredentials,
            ) -> Result<(), Error> {
                self.1 = None;

                // if self.0.is_started().map_err(to_err)? {
                //     self.0.stop_async().await.map_err(to_err)?;
                // }

                // self.0
                //     .set_configuration(&Configuration::Client(ClientConfiguration {
                //         ssid: creds.ssid.0.clone(),
                //         password: creds.password.clone(),
                //         ..Default::default()
                //     }))
                //     .map_err(to_err)?;

                // self.0.start_async().await.map_err(to_err)?;
                // self.0.connect_async().await.map_err(to_err)?;

                // self.1 = self
                //     .0
                //     .is_connected()
                //     .map_err(to_err)?
                //     .then_some(creds.ssid.clone());

                Ok(())
            }

            async fn connected_network(
                &mut self,
            ) -> Result<
                Option<
                    <<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId,
                >,
                Error,
            >{
                Ok(self.1.clone())
            }

            async fn stats(&mut self) -> Result<<Self::Data as WirelessData>::Stats, Error> {
                Ok(None)
            }
        }

        // fn to_err(_: WifiError) -> Error {
        //     Error::new(ErrorCode::NoNetworkInterface)
        // }
    }

    // TODO:
    // This adaptor would've not been necessary, if there was a common Wifi trait aggreed upon and
    // implemented by all MCU Wifi controllers in the field.
    //
    // Perhaps it is time to dust-off `embedded_svc::wifi` and publish it as a micro-crate?
    // `embedded-wifi`?
    #[cfg(feature = "esp")]
    pub mod esp {
        use esp_hal::peripheral::{Peripheral, PeripheralRef};
        use esp_wifi::wifi::{
            AuthMethod, ClientConfiguration, Configuration, ScanConfig, WifiController, WifiDevice,
            WifiError, WifiStaDevice,
        };

        use crate::matter::data_model::sdm::nw_commissioning::WiFiSecurity;
        use crate::matter::error::{Error, ErrorCode};
        use crate::matter::tlv::OctetsOwned;
        use crate::matter::utils::storage::Vec;
        use crate::stack::wireless::traits::{
            Controller, NetworkCredentials, WifiData, WifiScanResult, WifiSsid, WirelessData,
        };

        const MAX_NETWORKS: usize = 3;

        /// A `WifiDriverProvider` implementation for the ESP32 family of chips.
        pub struct EspWifiDriverProvider<'a, 'd> {
            controller: &'a esp_wifi::EspWifiController<'d>,
            peripheral: PeripheralRef<'d, esp_hal::peripherals::WIFI>,
        }

        impl<'a, 'd> EspWifiDriverProvider<'a, 'd> {
            /// Create a new instance of the `Esp32WifiDriverProvider` type.
            ///
            /// # Arguments
            /// - `controller` - The `esp-wifi` Wifi controller instance.
            /// - `peripheral` - The Wifi peripheral instance.
            pub fn new(
                controller: &'a esp_wifi::EspWifiController<'d>,
                peripheral: impl Peripheral<P = esp_hal::peripherals::WIFI> + 'd,
            ) -> Self {
                Self {
                    controller,
                    peripheral: peripheral.into_ref(),
                }
            }
        }

        impl super::WifiDriverProvider for EspWifiDriverProvider<'_, '_> {
            type Driver<'t>
                = WifiDevice<'t, WifiStaDevice>
            where
                Self: 't;
            type Controller<'t>
                = EspWifiController<'t>
            where
                Self: 't;

            async fn provide(&mut self) -> (Self::Driver<'_>, Self::Controller<'_>) {
                let (wifi_interface, controller) = esp_wifi::wifi::new_with_mode(
                    self.controller,
                    &mut self.peripheral,
                    WifiStaDevice,
                )
                .unwrap();

                (wifi_interface, EspWifiController::new(controller))
            }
        }

        /// An adaptor from the `esp-wifi` Wifi controller API to the `rs-matter` Wifi controller API
        pub struct EspWifiController<'a>(WifiController<'a>, Option<WifiSsid>);

        impl<'a> EspWifiController<'a> {
            /// Create a new instance of the `Esp32Controller` type.
            ///
            /// # Arguments
            /// - `controller` - The `esp-wifi` Wifi controller instance.
            pub const fn new(controller: WifiController<'a>) -> Self {
                Self(controller, None)
            }
        }

        impl Controller for EspWifiController<'_> {
            type Data = WifiData;

            async fn scan<F>(
                &mut self,
                network_id: Option<
                    &<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId,
                >,
                mut callback: F,
            ) -> Result<(), Error>
            where
                F: FnMut(Option<&<Self::Data as WirelessData>::ScanResult>) -> Result<(), Error>,
            {
                if !self.0.is_started().map_err(to_err)? {
                    self.0.start_async().await.map_err(to_err)?;
                }

                let mut scan_config = ScanConfig::default();
                if let Some(network_id) = network_id {
                    scan_config.ssid = Some(network_id.0.as_str());
                }

                let (aps, _) = self
                    .0
                    .scan_with_config_async::<MAX_NETWORKS>(scan_config)
                    .await
                    .map_err(to_err)?;

                for ap in aps {
                    callback(Some(&WifiScanResult {
                        ssid: WifiSsid(ap.ssid),
                        bssid: OctetsOwned {
                            vec: Vec::from_slice(&ap.bssid).unwrap(),
                        },
                        channel: ap.channel as _,
                        rssi: Some(ap.signal_strength),
                        band: None,
                        security: match ap.auth_method {
                            Some(AuthMethod::None) => WiFiSecurity::Unencrypted,
                            Some(AuthMethod::WEP) => WiFiSecurity::Wep,
                            Some(AuthMethod::WPA) => WiFiSecurity::WpaPersonal,
                            Some(AuthMethod::WPA3Personal) => WiFiSecurity::Wpa3Personal,
                            _ => WiFiSecurity::Wpa2Personal,
                        },
                    }))?;
                }

                callback(None)?;

                Ok(())
            }

            async fn connect(
                &mut self,
                creds: &<Self::Data as WirelessData>::NetworkCredentials,
            ) -> Result<(), Error> {
                self.1 = None;

                if self.0.is_started().map_err(to_err)? {
                    self.0.stop_async().await.map_err(to_err)?;
                }

                self.0
                    .set_configuration(&Configuration::Client(ClientConfiguration {
                        ssid: creds.ssid.0.clone(),
                        password: creds.password.clone(),
                        ..Default::default()
                    }))
                    .map_err(to_err)?;

                self.0.start_async().await.map_err(to_err)?;
                self.0.connect_async().await.map_err(to_err)?;

                self.1 = self
                    .0
                    .is_connected()
                    .map_err(to_err)?
                    .then_some(creds.ssid.clone());

                Ok(())
            }

            async fn connected_network(
                &mut self,
            ) -> Result<
                Option<
                    <<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId,
                >,
                Error,
            >{
                Ok(self.1.clone())
            }

            async fn stats(&mut self) -> Result<<Self::Data as WirelessData>::Stats, Error> {
                Ok(None)
            }
        }

        fn to_err(_: WifiError) -> Error {
            Error::new(ErrorCode::NoNetworkInterface)
        }
    }
}
