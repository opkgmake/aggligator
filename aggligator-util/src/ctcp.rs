use std::{
    fmt,
    io::{self, ErrorKind, Result},
    pin::Pin,
    ptr,
    task::{Context, Poll},
};

use aggligator::io::{StreamBox, TxRxBox};
use aggligator::transport::{AcceptingWrapper, ConnectingWrapper};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::{Sink, Stream};
use rand::{rngs::SmallRng, RngCore, SeedableRng};

/// 默认的 printable CTCP 包装名称。
const NAME: &str = "ctcp";

const HEADER_TSS: usize = 2;
const HEADER_MSS: usize = HEADER_TSS + 1;
const HEADER_XSS: usize = HEADER_MSS + 1;
const HEADER_MSS_MOD: u32 = (94 * 94 * 94) - 1;
const PRINTABLE_START: u8 = 0x20;
const PRINTABLE_END: u8 = 0x7e;

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
    working: BytesMut,
    packet: BytesMut,
    payload: BytesMut,
    encoded: BytesMut,
    length_buf: [u8; HEADER_XSS + HEADER_MSS],
    rng: SmallRng,
    short_only: bool,
}

impl CtcpTx {
    fn new(inner: Pin<Box<dyn Sink<Bytes, Error = io::Error> + Send + Sync + 'static>>, key: u32) -> Self {
        Self {
            inner,
            key,
            working: BytesMut::new(),
            packet: BytesMut::new(),
            payload: BytesMut::new(),
            encoded: BytesMut::new(),
            length_buf: [0; HEADER_XSS + HEADER_MSS],
            rng: SmallRng::from_entropy(),
            short_only: false,
        }
    }

    fn encode(&mut self, data: &[u8]) -> Result<Bytes> {
        if data.is_empty() {
            return Ok(Bytes::new());
        }

        if data.len() > u16::MAX as usize + 1 {
            return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 单帧负载过长"));
        }

        let key_byte = mask_key(self.key);

        let mut header = [0u8; HEADER_MSS];
        header[0] = random_range(&mut self.rng, 0x01, 0xff);
        let len_minus_one = (data.len() - 1) as u16;
        header[1] = (len_minus_one >> 8) as u8;
        header[2] = (len_minus_one & 0xff) as u8;

        let frame_key_full = self.key ^ header[0] as u32;
        let frame_key_byte = mask_key(frame_key_full);

        mask_bytes(&mut header[1..], frame_key_byte);
        shuffle_bytes(&mut header[1..], frame_key_full);
        delta_encode_in_place(&mut header[..], key_byte);

        self.working.clear();
        self.working.extend_from_slice(data);
        mask_bytes(&mut self.working[..], frame_key_byte);
        shuffle_bytes(&mut self.working[..], frame_key_full);
        delta_encode_in_place(&mut self.working[..], key_byte);

        self.packet.clear();
        self.packet.reserve(HEADER_MSS + self.working.len());
        self.packet.extend_from_slice(&header);
        self.packet.extend_from_slice(&self.working[..]);

        self.payload.clear();
        base94_encode_into(&self.packet[..], key_byte, &mut self.payload);

        let prefix_len = encode_length_prefix(
            self.payload.len(),
            self.key,
            &mut self.length_buf,
            &mut self.rng,
            &mut self.short_only,
        )?;

        self.encoded.clear();
        self.encoded.reserve(prefix_len + self.payload.len());
        self.encoded.extend_from_slice(&self.length_buf[..prefix_len]);
        self.encoded.extend_from_slice(&self.payload[..]);

        Ok(self.encoded.split_to(self.encoded.len()).freeze())
    }
}

impl Sink<Bytes> for CtcpTx {
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.inner.as_mut().poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Bytes) -> Result<()> {
        let encoded = {
            // SAFETY: we only mutate auxiliary buffers owned by `self` and never move the
            // pinned sink stored in `inner`.
            let this = unsafe { self.as_mut().get_unchecked_mut() };
            this.encode(&item)?
        };
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
    working: BytesMut,
    header_buf: [u8; HEADER_XSS + HEADER_MSS],
    short_only: bool,
}

impl CtcpRx {
    fn new(inner: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + Sync + 'static>>, key: u32) -> Self {
        Self { inner, key, working: BytesMut::new(), header_buf: [0; HEADER_XSS + HEADER_MSS], short_only: false }
    }

    fn decode(&mut self, frame: &[u8]) -> Result<Bytes> {
        if frame.is_empty() {
            return Ok(Bytes::new());
        }

        let (payload_ascii_len, prefix_len) =
            decode_length_prefix(frame, self.key, &mut self.header_buf, &mut self.short_only)?;

        if frame.len() < prefix_len + payload_ascii_len {
            return Err(io::Error::new(ErrorKind::UnexpectedEof, "CTCP 报文长度不足"));
        }

        if frame.len() != prefix_len + payload_ascii_len {
            return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 报文长度不匹配"));
        }

        let payload_ascii = &frame[prefix_len..prefix_len + payload_ascii_len];
        let key_byte = mask_key(self.key);

        self.working.clear();
        base94_decode_into(payload_ascii, key_byte, &mut self.working)?;

        if self.working.len() < HEADER_MSS {
            self.working.clear();
            return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 数据头不足"));
        }

        let mut payload = self.working.split_off(HEADER_MSS);
        let header_slice = &mut self.working[..];
        delta_decode_in_place(header_slice, key_byte);

        let frame_key_full = self.key ^ header_slice[0] as u32;
        let frame_key_byte = mask_key(frame_key_full);

        unshuffle_bytes(&mut header_slice[1..], frame_key_full);
        mask_bytes(&mut header_slice[1..], frame_key_byte);

        let expected_len = ((((header_slice[1] as usize) << 8) | (header_slice[2] as usize)) + 1) as usize;

        let payload_slice = &mut payload[..];
        delta_decode_in_place(payload_slice, key_byte);
        unshuffle_bytes(payload_slice, frame_key_full);
        mask_bytes(payload_slice, frame_key_byte);

        self.working.clear();

        if payload_slice.len() != expected_len {
            return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 负载长度不一致"));
        }

        Ok(payload.freeze())
    }
}

impl Stream for CtcpRx {
    type Item = Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                let decoded = {
                    let this = unsafe { self.as_mut().get_unchecked_mut() };
                    this.decode(&frame)
                };
                match decoded {
                    Ok(payload) => Poll::Ready(Some(Ok(payload))),
                    Err(err) => Poll::Ready(Some(Err(err))),
                }
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[inline(always)]
fn random_range(rng: &mut SmallRng, min: u8, max: u8) -> u8 {
    debug_assert!(min < max);
    let span = u32::from(max - min);
    let value = fast_mod_u32(rng.next_u32(), span);
    min.wrapping_add(value as u8)
}

#[inline(always)]
fn fast_mod_u32(value: u32, modulus: u32) -> u32 {
    debug_assert!(modulus > 0);
    ((value as u64 * modulus as u64) >> 32) as u32
}

fn encode_length_prefix(
    payload_len: usize, key: u32, buffer: &mut [u8; HEADER_XSS + HEADER_MSS], rng: &mut SmallRng,
    short_only: &mut bool,
) -> Result<usize> {
    if payload_len == 0 {
        return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 负载为空"));
    }

    if payload_len as u32 >= HEADER_MSS_MOD {
        return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 负载编码过长"));
    }

    let prefix = &mut buffer[..HEADER_XSS + HEADER_MSS];
    prefix.fill(PRINTABLE_START);

    let mut digits = [0u8; HEADER_MSS];
    let kf_mod = (key % HEADER_MSS_MOD) as u32;
    let mut n = ((payload_len as u32) + kf_mod) % HEADER_MSS_MOD;
    let dl = base94_decimal_encode(n, &mut digits);
    if dl == 0 || dl >= HEADER_XSS {
        return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 长度前缀非法"));
    }

    let start = HEADER_XSS - dl;
    prefix[start..HEADER_XSS].copy_from_slice(&digits[..dl]);

    let mut k = random_range(rng, PRINTABLE_START, PRINTABLE_END);
    let mut f = prefix[1];
    if f == PRINTABLE_START {
        if (k & 0x01) != 0 {
            k = k.wrapping_add(1);
        }
        f = random_range(rng, PRINTABLE_START, PRINTABLE_END);
    } else if (k & 0x01) == 0 {
        k = k.wrapping_add(1);
        if k > PRINTABLE_END {
            k = 0x21;
        }
    }

    prefix[0] = k;
    prefix[1] = f;
    prefix[..HEADER_XSS].swap(2, 3);

    if *short_only {
        return Ok(HEADER_XSS);
    }

    let checksum = u32::from(inet_checksum(&prefix[..HEADER_XSS]));
    n = ((checksum ^ (payload_len as u32)) + kf_mod) % HEADER_MSS_MOD;
    let extra_len = base94_decimal_encode(n, &mut digits);
    if extra_len != HEADER_MSS {
        return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 校验长度非法"));
    }

    let extra = &mut prefix[HEADER_XSS..HEADER_XSS + HEADER_MSS];
    extra.copy_from_slice(&digits);
    shuffle_bytes(extra, key);

    *short_only = true;
    Ok(HEADER_XSS + HEADER_MSS)
}

fn decode_length_prefix(
    data: &[u8], key: u32, buffer: &mut [u8; HEADER_XSS + HEADER_MSS], short_only: &mut bool,
) -> Result<(usize, usize)> {
    if *short_only {
        if data.len() < HEADER_XSS {
            return Err(io::Error::new(ErrorKind::UnexpectedEof, "CTCP 前缀不足"));
        }
        buffer[..HEADER_XSS].copy_from_slice(&data[..HEADER_XSS]);
        base94_decode_kf(&mut buffer[..HEADER_XSS]);
        let kf_mod = (key % HEADER_MSS_MOD) as u32;
        let raw = base94_decimal_decode(&buffer[1..1 + HEADER_MSS])?;
        let length = (raw + HEADER_MSS_MOD - kf_mod) % HEADER_MSS_MOD;
        if length == 0 {
            return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 长度为零"));
        }
        return Ok((length as usize, HEADER_XSS));
    }

    if data.len() < HEADER_XSS + HEADER_MSS {
        return Err(io::Error::new(ErrorKind::UnexpectedEof, "CTCP 前缀不足"));
    }

    buffer.copy_from_slice(&data[..HEADER_XSS + HEADER_MSS]);
    let checksum = u32::from(inet_checksum(&buffer[..HEADER_XSS]));
    base94_decode_kf(&mut buffer[..HEADER_XSS]);

    let kf_mod = (key % HEADER_MSS_MOD) as u32;
    let raw_length = base94_decimal_decode(&buffer[1..1 + HEADER_MSS])?;
    let length = (raw_length + HEADER_MSS_MOD - kf_mod) % HEADER_MSS_MOD;
    if length == 0 {
        return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 长度为零"));
    }

    let mut extra = [0u8; HEADER_MSS];
    extra.copy_from_slice(&buffer[HEADER_XSS..HEADER_XSS + HEADER_MSS]);
    unshuffle_bytes(&mut extra, key);
    let raw_verify = base94_decimal_decode(&extra)?;
    let verify = (raw_verify + HEADER_MSS_MOD - kf_mod) % HEADER_MSS_MOD;
    let expected = checksum ^ length;
    if verify != expected {
        return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 长度校验失败"));
    }

    *short_only = true;
    Ok((length as usize, HEADER_XSS + HEADER_MSS))
}

fn base94_decimal_encode(mut value: u32, out: &mut [u8; HEADER_MSS]) -> usize {
    if value == 0 {
        out[0] = PRINTABLE_START;
        return 1;
    }

    let mut digits = [0u8; HEADER_MSS];
    let mut len = 0;
    while value > 0 {
        digits[len] = (value % 94) as u8;
        value /= 94;
        len += 1;
    }

    for i in 0..len {
        out[len - 1 - i] = digits[i] + PRINTABLE_START;
    }

    len
}

fn base94_decimal_decode(data: &[u8]) -> Result<u32> {
    if data.is_empty() || data.len() > HEADER_MSS {
        return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 长度字段非法"));
    }

    let mut value = 0u32;
    for &byte in data {
        if !(PRINTABLE_START..=PRINTABLE_END).contains(&byte) {
            return Err(io::Error::new(ErrorKind::InvalidData, "非可打印 CTCP 字符"));
        }
        value = value * 94 + (byte - PRINTABLE_START) as u32;
    }

    Ok(value)
}

fn base94_decode_kf(header: &mut [u8]) {
    if header.len() >= HEADER_XSS {
        if (header[0] & 0x01) == 0 {
            header[1] = PRINTABLE_START;
        }
        header[0] = PRINTABLE_START;
        header.swap(2, 3);
    }
}

fn ip_standard_checksum(data: &[u8]) -> u16 {
    let mut acc: u32 = 0;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        let word = u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
        acc += word;
    }

    if let Some(&rem) = chunks.remainder().first() {
        acc += (rem as u32) << 8;
    }

    acc = (acc >> 16) + (acc & 0xffff);
    if (acc & 0xffff0000) != 0 {
        acc = (acc >> 16) + (acc & 0xffff);
    }

    acc as u16
}

fn inet_checksum(data: &[u8]) -> u16 {
    !ip_standard_checksum(data)
}

/// 从 32 位密钥中提取单字节掩码。
fn mask_key(key: u32) -> u8 {
    (key & 0xff) as u8
}

/// 逐字节与固定密钥异或。
fn mask_bytes(data: &mut [u8], key: u8) {
    if key == 0 {
        return;
    }

    const CHUNK: usize = std::mem::size_of::<u64>();
    let wide_key = u64::from_ne_bytes([key; CHUNK]);

    let mut chunks = data.chunks_exact_mut(CHUNK);
    for chunk in &mut chunks {
        let value = unsafe { ptr::read_unaligned(chunk.as_ptr() as *const u64) } ^ wide_key;
        unsafe { ptr::write_unaligned(chunk.as_mut_ptr() as *mut u64, value) };
    }

    for byte in chunks.into_remainder() {
        *byte ^= key;
    }
}

/// 使用与 openppp2 一致的顺序打乱字节。
fn shuffle_bytes(data: &mut [u8], key: u32) {
    let len = data.len();
    if len <= 1 {
        return;
    }

    let len_u32 = len as u32;
    let ptr = data.as_mut_ptr();
    for i in 0..len {
        let j = fast_mod_u32((i as u32) ^ key, len_u32) as usize;
        unsafe { ptr::swap(ptr.add(i), ptr.add(j)) };
    }
}

/// 根据打乱顺序反向恢复字节排列。
fn unshuffle_bytes(data: &mut [u8], key: u32) {
    let len = data.len();
    if len <= 1 {
        return;
    }

    let len_u32 = len as u32;
    let ptr = data.as_mut_ptr();
    for i in (0..len).rev() {
        let j = fast_mod_u32((i as u32) ^ key, len_u32) as usize;
        unsafe { ptr::swap(ptr.add(i), ptr.add(j)) };
    }
}

/// 差分编码实现，与 openppp2 的 `ssea::delta_encode` 一致。
fn delta_encode_in_place(data: &mut [u8], key: u8) {
    if data.is_empty() {
        return;
    }

    let mut prev = data[0];
    data[0] = data[0].wrapping_sub(key);
    for byte in &mut data[1..] {
        let current = *byte;
        *byte = current.wrapping_sub(prev);
        prev = current;
    }
}

/// 差分解码实现，对应 `ssea::delta_decode`。
fn delta_decode_in_place(data: &mut [u8], key: u8) {
    if data.is_empty() {
        return;
    }

    let mut current = data[0].wrapping_add(key);
    data[0] = current;
    for byte in &mut data[1..] {
        current = current.wrapping_add(*byte);
        *byte = current;
    }
}

/// Base94 编码，仅输出 0x20~0x7e 之间的可打印字符。
fn base94_encode_into(data: &[u8], key: u8, out: &mut BytesMut) {
    const BASE94: u8 = 94;
    const BASE93: u8 = BASE94 - 1;

    out.clear();

    let mut extra = 0usize;
    for &byte in data {
        if byte.wrapping_sub(key) >= BASE93 {
            extra += 1;
        }
    }

    let target_len = data.len() + extra;
    out.resize(target_len, 0);

    let mut offset = 0;
    let ptr = out.as_mut_ptr();
    for &byte in data {
        let adjusted = byte.wrapping_sub(key);
        unsafe {
            if adjusted >= BASE93 {
                let high = ((adjusted / BASE93) - 1) + BASE93;
                let low = adjusted % BASE93;
                ptr.add(offset).write(0x20 + high);
                ptr.add(offset + 1).write(0x20 + low);
                offset += 2;
            } else {
                ptr.add(offset).write(0x20 + adjusted);
                offset += 1;
            }
        }
    }

    unsafe {
        out.set_len(offset);
    }
}

/// Base94 解码，将可打印字符还原为原始字节。
fn base94_decode_into(data: &[u8], key: u8, out: &mut BytesMut) -> Result<()> {
    const BASE94: u8 = 94;
    const BASE93: u8 = BASE94 - 1;

    out.clear();

    let mut index = 0usize;
    let mut escapes = 0usize;
    let len = data.len();

    while index < len {
        let raw = data[index];
        if raw < PRINTABLE_START || raw > PRINTABLE_END {
            return Err(io::Error::new(ErrorKind::InvalidData, "非可打印 CTCP 字符"));
        }

        let value = raw - 0x20;
        if value >= BASE93 {
            index += 1;
            if index >= len {
                return Err(io::Error::new(ErrorKind::UnexpectedEof, "CTCP 高位缺失"));
            }

            let next_raw = data[index];
            if next_raw < PRINTABLE_START || next_raw > PRINTABLE_END {
                return Err(io::Error::new(ErrorKind::InvalidData, "非可打印 CTCP 字符"));
            }

            let next = next_raw - 0x20;
            if next >= BASE93 {
                return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 低位溢出"));
            }

            let combined = ((value - BASE93 + 1) as u16) * (BASE93 as u16) + (next as u16);
            if combined > 0xff {
                return Err(io::Error::new(ErrorKind::InvalidData, "CTCP 组合字节超界"));
            }

            escapes += 1;
        }

        index += 1;
    }

    out.resize(len - escapes, 0);

    index = 0;
    let mut out_offset = 0usize;
    let ptr = out.as_mut_ptr();

    while index < len {
        let raw = unsafe { *data.get_unchecked(index) };
        let value = raw - 0x20;

        unsafe {
            if value >= BASE93 {
                index += 1;
                let next_raw = *data.get_unchecked(index);
                let next = next_raw - 0x20;
                let combined = ((value - BASE93 + 1) as u16) * (BASE93 as u16) + (next as u16);
                ptr.add(out_offset).write((combined as u8).wrapping_add(key));
            } else {
                ptr.add(out_offset).write(value.wrapping_add(key));
            }
        }

        out_offset += 1;
        index += 1;
    }

    unsafe {
        out.set_len(out_offset);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;
    use rand::{rngs::SmallRng, SeedableRng};

    #[test]
    fn roundtrip_basic() {
        let data = b"Aggligator-CTCP";
        let sink =
            Box::pin(futures::sink::drain::<Bytes>().sink_map_err(|_| io::Error::new(ErrorKind::Other, "drain")));
        let mut tx = CtcpTx::new(sink, DEFAULT_KEY);
        let encoded = tx.encode(data).unwrap();
        assert!(encoded.iter().all(|b| (PRINTABLE_START..=PRINTABLE_END).contains(b)));

        let stream = Box::pin(futures::stream::empty::<Result<Bytes>>());
        let mut rx = CtcpRx::new(stream, DEFAULT_KEY);
        let decoded = rx.decode(&encoded).unwrap();
        assert_eq!(decoded.as_ref(), data);
    }

    #[test]
    fn roundtrip_with_binary_payload() {
        let data = [0u8, 255, 1, 2, 3, 128, 64, 33, 127];
        let sink =
            Box::pin(futures::sink::drain::<Bytes>().sink_map_err(|_| io::Error::new(ErrorKind::Other, "drain")));
        let mut tx = CtcpTx::new(sink, DEFAULT_KEY);
        let encoded = tx.encode(&data).unwrap();

        let stream = Box::pin(futures::stream::empty::<Result<Bytes>>());
        let mut rx = CtcpRx::new(stream, DEFAULT_KEY);
        let decoded = rx.decode(&encoded).unwrap();
        assert_eq!(decoded.as_ref(), data);
    }

    #[test]
    fn length_prefix_switches_to_short_mode_on_encode() {
        let key = DEFAULT_KEY;
        let mut buffer = [0u8; HEADER_XSS + HEADER_MSS];
        let mut rng = SmallRng::seed_from_u64(1);
        let mut short_only = false;

        let long = encode_length_prefix(128, key, &mut buffer, &mut rng, &mut short_only).unwrap();
        assert_eq!(long, HEADER_XSS + HEADER_MSS);
        assert!(short_only);

        let short = encode_length_prefix(128, key, &mut buffer, &mut rng, &mut short_only).unwrap();
        assert_eq!(short, HEADER_XSS);
        assert!(short_only);

        let short_again = encode_length_prefix(128, key, &mut buffer, &mut rng, &mut short_only).unwrap();
        assert_eq!(short_again, HEADER_XSS);
        assert!(short_only);
    }

    #[test]
    fn length_prefix_switches_to_short_mode_on_decode() {
        let key = DEFAULT_KEY;
        let mut encode_buffer = [0u8; HEADER_XSS + HEADER_MSS];
        let mut rng = SmallRng::seed_from_u64(2);
        let mut short_only = false;

        let prefix_long = encode_length_prefix(256, key, &mut encode_buffer, &mut rng, &mut short_only).unwrap();
        let long_bytes = encode_buffer[..prefix_long].to_vec();
        let prefix_short = encode_length_prefix(256, key, &mut encode_buffer, &mut rng, &mut short_only).unwrap();
        let short_bytes = encode_buffer[..prefix_short].to_vec();
        let prefix_short2 =
            encode_length_prefix(256, key, &mut encode_buffer, &mut rng, &mut short_only).unwrap();
        let short_bytes2 = encode_buffer[..prefix_short2].to_vec();

        let mut decode_buffer = [0u8; HEADER_XSS + HEADER_MSS];
        let mut short_only_flag = false;

        let (len1, used1) =
            decode_length_prefix(&long_bytes, key, &mut decode_buffer, &mut short_only_flag).unwrap();
        assert_eq!(len1, 256);
        assert_eq!(used1, HEADER_XSS + HEADER_MSS);
        assert!(short_only_flag);

        let (len2, used2) =
            decode_length_prefix(&short_bytes, key, &mut decode_buffer, &mut short_only_flag).unwrap();
        assert_eq!(len2, 256);
        assert_eq!(used2, HEADER_XSS);
        assert!(short_only_flag);

        let (len3, used3) =
            decode_length_prefix(&short_bytes2, key, &mut decode_buffer, &mut short_only_flag).unwrap();
        assert_eq!(len3, 256);
        assert_eq!(used3, HEADER_XSS);
        assert!(short_only_flag);
    }

    #[test]
    fn reject_invalid_symbols() {
        let invalid = [0x19, 0x7f];
        let stream = Box::pin(futures::stream::empty::<Result<Bytes>>());
        let mut rx = CtcpRx::new(stream, DEFAULT_KEY);
        let err = rx.decode(&invalid).unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::InvalidData | ErrorKind::UnexpectedEof));
    }

    #[test]
    fn inet_checksum_matches_reference() {
        assert_eq!(inet_checksum(&[0x3e, 0x2f, 0x28, 0x51]), 0x997f);
    }
}

impl fmt::Display for CtcpWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", NAME)
    }
}
