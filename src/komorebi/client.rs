//! Client for sending commands to komorebi.

#[cfg(not(test))]
use anyhow::Context;
use anyhow::Result;
use komorebi_client::SocketMessage;
#[cfg(not(test))]
use komorebi_client::send_message;

#[cfg(not(test))]
pub fn send_command(msg: SocketMessage) -> Result<()> {
    send_message(&msg).context("send message to komorebi")?;
    Ok(())
}

#[cfg(test)]
pub fn send_command(_msg: SocketMessage) -> Result<()> {
    // In unit tests, avoid sending IPC messages to the live komorebi pipe.
    Ok(())
}
