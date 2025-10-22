//! 速度测试。

use futures::future;
use rand::prelude::*;
use rand_xoshiro::Xoroshiro128StarStar;
use std::{
    io::{Error, ErrorKind, Result},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    join, select,
    sync::{mpsc, oneshot, watch},
    time::Instant,
};
use tracing::Instrument;

use aggligator::exec;

const BUF_SIZE: usize = 8192;
const MB: f64 = 1_048_576.;

/// 速度报告的时间间隔。
pub const INTERVAL: Duration = Duration::from_secs(10);

/// Performs a speed test.
///
/// `name` specifies the test name for logging.
///
/// `read` and `write` are the read- and write-halves of the connection that should be tested.
///
/// `limit` and `duration` specify optional limits for the amount of data sent and test duration.
///
/// `send` and `receive` specify whether sending and receiving data should be performed.
///
/// If `recv_block` is true, the receive-half is held open but no data is read.
///
/// `report_interval` specifies the speed reporting interval.
///
/// The optional channel `speed_tx` is used to report the measured speed during the
/// running test.
///
/// The function returns the measured send and receive speeds in bytes per second.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all, fields(name =% name))]
pub async fn speed_test(
    name: &str, mut read: impl AsyncRead + Unpin + Send + 'static,
    mut write: impl AsyncWrite + Unpin + Send + 'static, limit: Option<usize>, duration: Option<Duration>,
    send: bool, receive: bool, recv_block: bool, report_interval: Duration,
    speed_tx: Option<watch::Sender<(f64, f64)>>,
) -> Result<(f64, f64)> {
    let start = Instant::now();
    let (stop_tx, _stop_rx) = mpsc::channel::<()>(1);

    let (send_tx, recv_tx) = match speed_tx {
        Some(speed_tx) => {
            let (send_tx, send_rx) = watch::channel(0.);
            let (recv_tx, recv_rx) = watch::channel(0.);
            let mut send_rx = Some(send_rx);
            let mut recv_rx = Some(recv_rx);
            exec::spawn(async move {
                while send_rx.is_some() || recv_rx.is_some() {
                    let send = send_rx.as_mut().map(|rx| *rx.borrow_and_update()).unwrap_or_default();
                    let recv = recv_rx.as_mut().map(|rx| *rx.borrow_and_update()).unwrap_or_default();

                    if speed_tx.send((send, recv)).is_err() {
                        break;
                    }

                    select! {
                        res = async {
                            match &mut send_rx {
                                Some(rx) => rx.changed().await,
                                None => future::pending().await,
                            }
                        } => if res.is_err() { send_rx = None},
                        res = async {
                            match &mut recv_rx {
                                Some(rx) => rx.changed().await,
                                None => future::pending().await,
                            }
                        } => if res.is_err() { recv_rx = None},
                    }
                }
            });
            (Some(send_tx), Some(recv_tx))
        }
        None => (None, None),
    };

    tracing::info!("开始测速");
    #[cfg(debug_assertions)]
    tracing::warn!("调试构建，测速结果仅供参考");

    let sender_stop_tx = stop_tx.clone();
    let (stop_sender_tx, mut stop_sender_rx) = oneshot::channel();
    let sender = exec::spawn(
        async move {
            if !send {
                return Ok((0, Duration::ZERO));
            }

            let seed = rand::random();
            write.write_u64(seed).await?;
            let mut rng = Xoroshiro128StarStar::seed_from_u64(seed);

            let mut sent_total = 0;
            let mut sent_interval = 0;
            let mut interval_start = Instant::now();

            #[allow(clippy::assertions_on_constants)]
            while limit.map(|limit| sent_total <= limit).unwrap_or(true)
                && !sender_stop_tx.is_closed()
                && start.elapsed() < duration.unwrap_or(Duration::MAX)
            {
                assert!(BUF_SIZE % 8 == 0);
                let mut buf = [0; BUF_SIZE];
                rng.fill_bytes(&mut buf);

                write.write_all(&buf).await?;

                sent_total += BUF_SIZE;
                sent_interval += BUF_SIZE;

                if interval_start.elapsed() >= report_interval {
                    let speed = sent_interval as f64 / interval_start.elapsed().as_secs_f64();

                    tracing::info!("发送速率：{:.1} MB/s", speed / MB);
                    if let Some(tx) = &send_tx {
                        if tx.send(speed).is_err() {
                            break;
                        }
                    }

                    sent_interval = 0;
                    interval_start = Instant::now();
                }

                if let Ok(()) = stop_sender_rx.try_recv() {
                    break;
                }
            }

            tracing::info!("发送端结束");
            Ok::<_, Error>((sent_total, start.elapsed()))
        }
        .in_current_span(),
    );

    let receiver = exec::spawn(
        async move {
            if !receive {
                return Ok((0, Duration::ZERO));
            }

            let remote_seed = read.read_u64().await?;
            let mut rng = Xoroshiro128StarStar::seed_from_u64(remote_seed);

            if recv_block {
                stop_tx.closed().await;
                return Ok((0, Duration::ZERO));
            }

            let mut recved_total = 0;
            let mut recved_interval = 0;
            let mut interval_start = Instant::now();

            while !stop_tx.is_closed() && start.elapsed() < duration.unwrap_or(Duration::MAX) {
                let mut buf = [0; BUF_SIZE];
                let mut n = read.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                match n % 8 {
                    0 => (),
                    rem => {
                        n += read.read_exact(&mut buf[n..(n + 8 - rem)]).await?;
                        if n % 8 != 0 {
                            break;
                        }
                    }
                }
                let buf = &buf[..n];

                let mut chk_buf = vec![0; n];
                assert!(n % 8 == 0);
                rng.fill_bytes(&mut chk_buf);
                if chk_buf != buf {
                    let _ = stop_sender_tx.send(());
                    return Err(Error::new(ErrorKind::InvalidData, "收到的数据格式错误"));
                }

                recved_total += n;
                recved_interval += n;

                if interval_start.elapsed() >= report_interval {
                    let speed = recved_interval as f64 / interval_start.elapsed().as_secs_f64();

                    tracing::info!("接收速率：{:.1} MB/s", speed / MB);
                    if let Some(tx) = &recv_tx {
                        if tx.send(speed).is_err() {
                            break;
                        }
                    }

                    recved_interval = 0;
                    interval_start = Instant::now();
                }
            }

            tracing::info!("接收端结束");
            Ok((recved_total, start.elapsed()))
        }
        .in_current_span(),
    );

    let (Ok(sender), Ok(receiver)) = join!(sender, receiver) else { unreachable!() };

    if let Err(err) = &sender {
        tracing::warn!(%err, "发送端出错");
    }
    if let Err(err) = &receiver {
        tracing::warn!(%err, "接收端出错");
    }

    let (tx_total, tx_dur) = sender?;
    let tx_speed = tx_total as f64 / tx_dur.as_secs_f64().max(1e-10);

    let (rx_total, rx_dur) = receiver?;
    let rx_speed = rx_total as f64 / rx_dur.as_secs_f64().max(1e-10);

    tracing::info!("上行：{tx_speed:.0} 字节/秒  下行：{rx_speed:.0} 字节/秒");
    Ok((tx_speed, rx_speed))
}
