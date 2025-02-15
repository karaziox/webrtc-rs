#[cfg(test)]
mod allocation_manager_test;

use super::*;
use crate::error::*;
use crate::relay::*;

use futures::future;
use std::collections::HashMap;
use stun::textattrs::Username;
use util::Conn;

// ManagerConfig a bag of config params for Manager.
pub struct ManagerConfig {
    pub relay_addr_generator: Box<dyn RelayAddressGenerator + Send + Sync>,
}

// Manager is used to hold active allocations
pub struct Manager {
    allocations: AllocationMap,
    reservations: Arc<Mutex<HashMap<String, u16>>>,
    relay_addr_generator: Box<dyn RelayAddressGenerator + Send + Sync>,
}

impl Manager {
    // creates a new instance of Manager.
    pub fn new(config: ManagerConfig) -> Self {
        Manager {
            allocations: Arc::new(Mutex::new(HashMap::new())),
            reservations: Arc::new(Mutex::new(HashMap::new())),
            relay_addr_generator: config.relay_addr_generator,
        }
    }

    // Close closes the manager and closes all allocations it manages
    pub async fn close(&self) -> Result<()> {
        let allocations = self.allocations.lock().await;
        for a in allocations.values() {
            a.close().await?;
        }
        Ok(())
    }

    // get_allocation fetches the allocation matching the passed FiveTuple
    pub async fn get_allocation(&self, five_tuple: &FiveTuple) -> Option<Arc<Allocation>> {
        let allocations = self.allocations.lock().await;
        allocations.get(five_tuple).map(Arc::clone)
    }

    // create_allocation creates a new allocation and starts relaying
    pub async fn create_allocation(
        &self,
        five_tuple: FiveTuple,
        turn_socket: Arc<dyn Conn + Send + Sync>,
        requested_port: u16,
        lifetime: Duration,
        username: Username,
    ) -> Result<Arc<Allocation>> {
        if lifetime == Duration::from_secs(0) {
            return Err(Error::ErrLifetimeZero);
        }

        if self.get_allocation(&five_tuple).await.is_some() {
            return Err(Error::ErrDupeFiveTuple);
        }

        let (relay_socket, relay_addr) = self
            .relay_addr_generator
            .allocate_conn(true, requested_port)
            .await?;
        let mut a = Allocation::new(
            turn_socket,
            relay_socket,
            relay_addr,
            five_tuple.clone(),
            username,
        );
        a.allocations = Some(Arc::clone(&self.allocations));

        log::debug!("listening on relay addr: {:?}", a.relay_addr);
        a.start(lifetime).await;
        a.packet_handler().await;

        let a = Arc::new(a);
        {
            let mut allocations = self.allocations.lock().await;
            allocations.insert(five_tuple, Arc::clone(&a));
        }

        Ok(a)
    }

    // delete_allocation removes an allocation
    pub async fn delete_allocation(&self, five_tuple: &FiveTuple) {
        let allocation = self.allocations.lock().await.remove(five_tuple);

        if let Some(a) = allocation {
            if let Err(err) = a.close().await {
                log::error!("Failed to close allocation: {}", err);
            }
        }
    }

    /// Deletes the [`Allocation`]s according to the specified `username`.
    pub async fn delete_allocations_by_username(&self, name: &str) {
        let to_delete = {
            let mut allocations = self.allocations.lock().await;

            let mut to_delete = Vec::new();

            // TODO(logist322): Use `.drain_filter()` once stabilized.
            allocations.retain(|_, allocation| {
                let match_name = allocation.username.text == name;

                if match_name {
                    to_delete.push(Arc::clone(allocation));
                }

                !match_name
            });

            to_delete
        };

        future::join_all(to_delete.iter().map(|a| async move {
            if let Err(err) = a.close().await {
                log::error!("Failed to close allocation: {}", err);
            }
        }))
        .await;
    }

    // create_reservation stores the reservation for the token+port
    pub async fn create_reservation(&self, reservation_token: String, port: u16) {
        let reservations = Arc::clone(&self.reservations);
        let reservation_token2 = reservation_token.clone();

        tokio::spawn(async move {
            let sleep = tokio::time::sleep(Duration::from_secs(30));
            tokio::pin!(sleep);
            tokio::select! {
                _ = &mut sleep => {
                    let mut reservations = reservations.lock().await;
                    reservations.remove(&reservation_token2);
                },
            }
        });

        let mut reservations = self.reservations.lock().await;
        reservations.insert(reservation_token, port);
    }

    // get_reservation returns the port for a given reservation if it exists
    pub async fn get_reservation(&self, reservation_token: &str) -> Option<u16> {
        let reservations = self.reservations.lock().await;
        reservations.get(reservation_token).copied()
    }

    // get_random_even_port returns a random un-allocated udp4 port
    pub async fn get_random_even_port(&self) -> Result<u16> {
        let (_, addr) = self.relay_addr_generator.allocate_conn(true, 0).await?;
        Ok(addr.port())
    }
}
