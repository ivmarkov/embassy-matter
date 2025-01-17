use edge_nal::UdpBind;
use edge_nal_embassy::{Udp, UdpBuffers, UdpError};

use embassy_futures::select::select;
use embassy_net::Stack;
use embassy_time::{Duration, Timer};

use rs_matter::error::Error;
use rs_matter_stack::netif::{Netif, NetifConf};

use crate::error::to_net_error;

const TIMEOUT_PERIOD_SECS: u8 = 5;

/// A `Netif` and `UdpBind` traits implementation for Embassy
/// (`embassy-net` in particular)
pub struct EmbassyNetif<'d, const N: usize, const TX_SZ: usize, const RX_SZ: usize, const M: usize>
{
    stack: Stack<'d>,
    udp: Udp<'d, N, TX_SZ, RX_SZ, M>,
}

impl<'d, const N: usize, const TX_SZ: usize, const RX_SZ: usize, const M: usize>
    EmbassyNetif<'d, N, TX_SZ, RX_SZ, M>
{
    /// Create a new `EmbassyNetif` instance
    pub fn new(stack: Stack<'d>, buffers: &'d UdpBuffers<N, TX_SZ, RX_SZ, M>) -> Self {
        Self {
            stack,
            udp: Udp::new(stack, buffers),
        }
    }

    fn get_conf(&self) -> Result<NetifConf, ()> {
        let Some(v4) = self.stack.config_v4() else {
            return Err(());
        };

        let Some(v6) = self.stack.config_v6() else {
            return Err(());
        };

        let conf = NetifConf {
            ipv4: v4.address.address(),
            ipv6: v6.address.address(),
            interface: 0,
            mac: [0; 6], // TODO
        };

        Ok(conf)
    }

    async fn wait_conf_change(&self) -> Result<(), ()> {
        // Embassy does have a `wait_config_up` but no `wait_config_change` or `wait_config_down`
        // Use a timer as a workaround

        let wait_up = self.stack.wait_config_up();
        let timer = Timer::after(Duration::from_secs(TIMEOUT_PERIOD_SECS as _));

        select(wait_up, timer).await;

        Ok(())
    }
}

impl<const N: usize, const TX_SZ: usize, const RX_SZ: usize, const M: usize> Netif
    for EmbassyNetif<'_, N, TX_SZ, RX_SZ, M>
{
    async fn get_conf(&self) -> Result<Option<NetifConf>, Error> {
        Ok(EmbassyNetif::get_conf(self).ok())
    }

    async fn wait_conf_change(&self) -> Result<(), Error> {
        EmbassyNetif::wait_conf_change(self)
            .await
            .map_err(to_net_error)?;

        Ok(())
    }
}

impl<const N: usize, const TX_SZ: usize, const RX_SZ: usize, const M: usize> UdpBind
    for EmbassyNetif<'_, N, TX_SZ, RX_SZ, M>
{
    type Error = UdpError;

    type Socket<'b>
        = edge_nal_embassy::UdpSocket<'b, N, TX_SZ, RX_SZ, M>
    where
        Self: 'b;

    async fn bind(&self, local: core::net::SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
        self.udp.bind(local).await
    }
}
