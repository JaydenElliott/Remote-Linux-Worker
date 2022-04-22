//! Exposes the command processing logic to the job module.

use crate::errors::RLWServerError;

use std::process::{ExitStatus, Stdio};
use std::sync::mpsc::Sender;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

// Path to the directory where the processes will be run.
// TODO: Make this a configurable part of the server.
const PROCESS_DIR_PATH: &str = "./tests/test_env";

// Upper limit on size of chunks sent down output channel
const OUTPUT_CHUNK_SIZE_BYTES: usize = 1024;

/// Executes a command using the arguments provided and sends the output results down the provided channel.
///
/// # Arguments
///
/// * `command`   - Command to execute. Examples: "cargo", "ls", "/bin/bash".
/// * `args`      - Arguments to accompany the command. Examples: "--version", "-a", "./file.sh".
/// * `tx_pid`    - The channel producer used to send the process PID of the job started.
/// * `tx_output` - The channel producer used to stream the command results

pub async fn execute_command(
    command: String,
    args: Vec<String>,
    tx_pid: Option<Sender<u32>>,
    tx_output: Sender<Vec<u8>>,
) -> Result<ExitStatus, RLWServerError> {
    // Start process
    let mut output = Command::new(command)
        .args(args)
        .current_dir(PROCESS_DIR_PATH)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    // Send PID
    if let Some(t) = tx_pid {
        t.send(
            output
                .id()
                .ok_or(RLWServerError("Unable to get process pid".to_string()))?,
        )?;
    }

    // Setup stream readers
    let mut stdout_reader = BufReader::with_capacity(
        OUTPUT_CHUNK_SIZE_BYTES,
        output
            .stdout
            .take()
            .ok_or_else(|| RLWServerError("Unable to read from stdout stream".to_string()))?,
    );

    let mut stderr_reader = BufReader::new(
        output
            .stderr
            .take()
            .ok_or_else(|| RLWServerError("Unable to read from stderr stream".to_string()))?,
    );

    // // Read from stderr and send the output down the channel
    let tx_output_err = tx_output.clone();
    let thread: JoinHandle<Result<(), RLWServerError>> = tokio::spawn(async move {
        let mut buf = [0u8; OUTPUT_CHUNK_SIZE_BYTES];
        while let Ok(size) = stderr_reader.read(&mut buf[..]).await {
            // Indicates end of stream
            if size == 0 {
                break;
            }
            tx_output_err.send(buf[0..size].to_vec())?;

            // Reset buffer
            buf = [0u8; OUTPUT_CHUNK_SIZE_BYTES];
        }
        Ok(())
    });

    let mut buf = [0u8; OUTPUT_CHUNK_SIZE_BYTES];
    while let Ok(size) = stdout_reader.read(&mut buf[..]).await {
        //     // Indicates end of stream
        if size == 0 {
            break;
        }
        tx_output.send(buf[0..size].to_vec())?;

        // Reset buffer
        buf = [0u8; OUTPUT_CHUNK_SIZE_BYTES];
    }

    if let Err(e) = thread.await {
        return Err(RLWServerError(format!(
            "Error with output reader thread join: {:?}",
            e
        )));
    }

    // Return exit code or terminating signal
    let status = output.wait().await?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};

    const TESTING_SCRIPTS_DIR: &str = "../scripts/";

    /// Tests the execution of a new start command and the resulting output.
    ///
    /// Files used: tests/scripts/start_process.sh
    #[tokio::test(flavor = "multi_thread")]
    async fn test_command_processing() -> Result<(), RLWServerError> {
        // Setup
        let (tx_output, rx_output): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::channel();
        let (tx_pid, rx_pid): (Sender<u32>, Receiver<u32>) = mpsc::channel();
        let command = "/bin/bash".to_string();
        let args = vec![TESTING_SCRIPTS_DIR.to_string() + "start_process.sh"];

        // Test command execution
        let t1: JoinHandle<Result<(), RLWServerError>> = tokio::spawn(async move {
            execute_command(command, args, Some(tx_pid), tx_output).await?;
            Ok(())
        });

        // Test PID received successfully
        rx_pid.recv()?;

        // Test output received successfully
        let mut output: Vec<u8> = Vec::new();
        for byte in rx_output {
            output.extend(byte);
        }

        // Test no errors in execute_command()
        t1.await
            .map_err(|e| RLWServerError(format!("Error when executing command: {:?}", e)))??;

        // Test output was as expected
        let str_result = std::str::from_utf8(&output)
            .map_err(|_| RLWServerError("Failed to map result to utf8 str".to_string()))?;
        assert_eq!(str_result, "temp file removed\ntemp file created\n");

        Ok(())
    }

    /// Tests if an invalid command raises an error correctly
    #[tokio::test]
    async fn test_incorrect_command() -> Result<(), RLWServerError> {
        // Setup
        let (tx_output, _rx_output): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::channel();
        let (tx_pid, _rx_input): (Sender<u32>, Receiver<u32>) = mpsc::channel();
        let command = "!i_am_a_bad_command!".to_string();
        let args = vec!["-abc".to_string()];

        // Expected failure: "No such file or directory (os error 2)"
        assert!(execute_command(command, args, Some(tx_pid), tx_output)
            .await
            .is_err());
        Ok(())
    }
}
