use std::{
    fmt,
    io::{self, ErrorKind, Result},
    pin::Pin,
    task::{Context, Poll},
};

use aggligator::io::{StreamBox, TxRxBox};
use aggligator::transport::{AcceptingWrapper, ConnectingWrapper};
use async_trait::async_trait;
use bytes::{BufMut, Bytes, BytesMut};
use futures::{Sink, Stream};

/// 默认的 printable CTCP 包装名称。
const NAME: &str = "ctcp";

/// openppp2 CTCP 管道默认的密钥常量。
///
/// 这是仓库 `liulilittle/openppp2` 中 `AppConfiguration` 的默认值，
/// 包括混淆（shuffle）、掩码与差分编码所依赖的主密钥。
pub const DEFAULT_KEY: u32 = 154_543_927;

/// 为 `agg-tunnel` 提供可打印纯文本传输的包装器。
///
/// 该包装器复用 openppp2 项目中 CTCP 的核心思路：
/// - 使用固定密钥对报文做差分编码；
/// - 按密钥派生的顺序打乱字节位置；
/// - 与掩码异或后再执行 Base94 编码，确保传输内容仅包含可打印字符。
///
/// 由于 `Aggligator` 的链路在进入聚合层前已经完成可靠分帧，
/// 因此这里直接在每个聚合帧上套用 CTCP 算法，实现端到端的 printable 加密。
#[derive(Debug, Clone, Copy)]
pub struct CtcpWrapper {
    key: u32,
}

impl Default for CtcpWrapper {
    fn default() -> Self {
        Self::new()
    }
}

impl CtcpWrapper {
    /// 创建一个使用默认 CTCP 密钥的包装器实例。
    pub const fn new() -> Self {
        Self::with_key(DEFAULT_KEY)
    }

    /// 使用自定义 CTCP 密钥创建包装器。
    pub const fn with_key(key: u32) -> Self {
        Self { key }
    }

    /// 返回包装器当前使用的密钥。
    pub const fn key(&self) -> u32 {
        self.key
    }
}

#[async_trait]
impl ConnectingWrapper for CtcpWrapper {
    fn name(&self) -> &str {
        NAME
    }

    async fn wrap(&self, stream: StreamBox) -> Result<StreamBox> {
        Ok(wrap_stream(stream, self.key))
    }
}

#[async_trait]
impl AcceptingWrapper for CtcpWrapper {
    fn name(&self) -> &str {
        NAME
    }

    async fn wrap(&self, stream: StreamBox) -> Result<StreamBox> {
        Ok(wrap_stream(stream, self.key))
    }
}

/// 将底层流包装为 CTCP printable 传输。
fn wrap_stream(stream: StreamBox, key: u32) -> StreamBox {
    let tx_rx = stream.into_tx_rx();
    let (tx, rx) = tx_rx.into_split();
    let tx = CtcpTx::new(tx, key);
    let rx = CtcpRx::new(rx, key);
    TxRxBox::new(tx, rx).into()
}

/// 负责编码发送侧报文。
struct CtcpTx {
    inner: Pin<Box<dyn Sink<Bytes, Error = io::Error> + Send + Sync + 'static>>,
    key: u32,
}

impl CtcpTx {
    fn new(inner: Pin<Box<dyn Sink<Bytes, Error = io::Error> + Send + Sync + 'static>>, key: u32) -> Self {
        Self { inner, key }
    }
}

impl Sink<Bytes> for CtcpTx {
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.inner.as_mut().poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Bytes) -> Result<()> {
        let encoded = encode_frame(&item, self.key);
        self.inner.as_mut().start_send(encoded)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.inner.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.inner.as_mut().poll_close(cx)
    }
}

/// 负责解码接收侧报文。
struct CtcpRx {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + Sync + 'static>>,
    key: u32,
}

impl CtcpRx {
    fn new(inner: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + Sync + 'static>>, key: u32) -> Self {
        Self { inner, key }
    }
}

impl Stream for CtcpRx {
    type Item = Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(frame))) => match decode_frame(&frame, self.key) {
                Ok(decoded) => Poll::Ready(Some(Ok(decoded))),
                Err(err) => Poll::Ready(Some(Err(err))),
            },
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// 对单个聚合帧执行 CTCP 编码。
fn encode_frame(data: &[u8], key: u32) -> Bytes {
    if data.is_empty() {
        return Bytes::new();
    }

    let key_byte = mask_key(key);
    let mut working = data.to_vec();
    mask_bytes(&mut working, key_byte);
    shuffle_bytes(&mut working, key);
    let delta = delta_encode(&working, key_byte);
    base94_encode(&delta, key_byte)
}

/// 对单个聚合帧执行 CTCP 解码。
fn decode_frame(data: &[u8], key: u32) -> Result<Bytes> {
    if data.is_empty() {
        return Ok(Bytes::new());
    }

    let key_byte = mask_key(key);
    let delta = base94_decode(data, key_byte)?;
    let mut restored = delta_decode(&delta, key_byte);
    unshuffle_bytes(&mut restored, key);
    mask_bytes(&mut restored, key_byte);
    Ok(Bytes::from(restored))
}

/// 从 32 位密钥中提取单字节掩码。
fn mask_key(key: u32) -> u8 {
    (key & 0xff) as u8
}

/// 逐字节与固定密钥异或。
fn mask_bytes(data: &mut [u8], key: u8) {
    for byte in data.iter_mut() {
        *byte ^= key;
    }
}

/// 使用与 openppp2 一致的顺序打乱字节。
fn shuffle_bytes(data: &mut [u8], key: u32) {
    let len = data.len();
    if len == 0 {
        return;
    }

    for i in 0..len {
        let j = ((i as u32) ^ key) % (len as u32);
        data.swap(i, j as usize);
    }
}

/// 根据打乱顺序反向恢复字节排列。
fn unshuffle_bytes(data: &mut [u8], key: u32) {
    let len = data.len();
    if len == 0 {
        return;
    }

    for i in (0..len).rev() {
        let j = ((i as u32) ^ key) % (len as u32);
        data.swap(i, j as usize);
    }
}

/// 差分编码实现，与 openppp2 的 `ssea::delta_encode` 一致。
fn delta_encode(data: &[u8], key: u8) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(data.len());
    let mut iter = data.iter();
    let first = iter.next().copied().unwrap();
    out.push(first.wrapping_sub(key));

    let mut prev = first;
    for current in iter {
        out.push(current.wrapping_sub(prev));
        prev = *current;
    }

    out
}

/// 差分解码实现，对应 `ssea::delta_decode`。
fn delta_decode(data: &[u8], key: u8) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(data.len());
    let mut iter = data.iter();
    let first = iter.next().copied().unwrap();
    let mut current = first.wrapping_add(key);
    out.push(current);

    for delta in iter {
        current = current.wrapping_add(*delta);
        out.push(current);
    }

    out
}

/// Base94 编码，仅输出 0x20~0x7e 之间的可打印字符。
fn base94_encode(data: &[u8], key: u8) -> Bytes {
    const BASE94: u8 = 94;
    const BASE93: u8 = BASE94 - 1;

    let mut out = BytesMut::with_capacity(data.len() * 2);
    for &byte in data {
        let adjusted = byte.wrapping_sub(key);
        if adjusted >= BASE93 {
            let high = ((adjusted / BASE93) - 1) + BASE93;
            let low = adjusted % BASE93;
            out.put_u8(0x20 + high);
            out.put_u8(0x20 + low);
        } else {
            out.put_u8(0x20 + adjusted);
        }
    }

    out.freeze()
}

/// Base94 解码，将可打印字符还原为原始字节。
fn base94_decode(data: &[u8], key: u8) -> Result<Vec<u8>> {
    const BASE94: u8 = 94;
    const BASE93: u8 = BASE94 - 1;

    let mut out = Vec::with_capacity(data.len());
    let mut index = 0;

    while index < data.len() {
        let mut value = data[index];
        if value < 0x20 {
            return Err(io::Error::new(ErrorKind::InvalidData, "非可打印 CTCP 字符"));
        }

        value -= 0x20;
        if value > BASE94 {
            return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 符号溢出"));
        }

        if value >= BASE93 {
            index += 1;
            if index >= data.len() {
                return Err(io::Error::new(ErrorKind::UnexpectedEof, "CTCP 高位缺失"));
            }

            let mut next = data[index];
            if next < 0x20 {
                return Err(io::Error::new(ErrorKind::InvalidData, "非可打印 CTCP 字符"));
            }
            next -= 0x20;
            if next > BASE93 {
                return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 低位溢出"));
            }

            let combined = ((value - BASE93 + 1) as u16) * (BASE93 as u16) + (next as u16);
            if combined > 0xff {
                return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 组合字节超界"));
            }

            out.push((combined as u8).wrapping_add(key));
        } else {
            out.push(value.wrapping_add(key));
        }

        index += 1;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_basic() {
        let data = b"Aggligator-CTCP";
        let encoded = encode_frame(data, DEFAULT_KEY);
        assert!(encoded.iter().all(|b| (0x20..=0x7e).contains(b)));
        let decoded = decode_frame(&encoded, DEFAULT_KEY).unwrap();
        assert_eq!(decoded.as_ref(), data);
    }

    #[test]
    fn roundtrip_with_binary_payload() {
        let data = [0u8, 255, 1, 2, 3, 128, 64, 33, 127];
        let encoded = encode_frame(&data, DEFAULT_KEY);
        let decoded = decode_frame(&encoded, DEFAULT_KEY).unwrap();
        assert_eq!(decoded.as_ref(), data);
    }

    #[test]
    fn reject_invalid_symbols() {
        let invalid = [0x19, 0x7f];
        let err = decode_frame(&invalid, DEFAULT_KEY).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }
}

impl fmt::Display for CtcpWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", NAME)
    }
}
