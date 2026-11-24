//! Just enough OTLP/HTTP protobuf to read a logs export (`ExportLogsServiceRequest`,
//! opentelemetry-proto `collector/logs/v1` and `logs/v1`), written by hand so the collector has
//! no protobuf dependency. Only the fields PM OS reads are decoded; every other field is skipped
//! by its wire type. Anything malformed is an error, and the whole request is refused.
//!
//! The encoder is for tests (the stand-in stream): it writes the same fields.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// One log record: its time, its attributes and its body, as JSON.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogRecord {
    pub time_unix_nano: u64,
    pub event_name: Option<String>,
    pub body: Option<Value>,
    pub attributes: BTreeMap<String, Value>,
    /// The resource's attributes (OpenShell's sandbox id may be there).
    pub resource: BTreeMap<String, Value>,
}

/// The most nesting of `AnyValue` read.
const MAX_DEPTH: usize = 16;

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

type R<T> = Result<T, String>;

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn varint(&mut self) -> R<u64> {
        let mut out = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos).ok_or("truncated varint")?;
            self.pos += 1;
            out |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(out);
            }
        }
        Err("varint too long".into())
    }

    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let s = self.buf.get(self.pos..end).ok_or("truncated field")?;
        self.pos = end;
        Ok(s)
    }

    fn key(&mut self) -> R<(u64, u8)> {
        let k = self.varint()?;
        Ok((k >> 3, (k & 7) as u8))
    }

    fn bytes(&mut self) -> R<&'a [u8]> {
        let n = usize::try_from(self.varint()?).map_err(|_| "length overflow")?;
        self.take(n)
    }

    fn fixed64(&mut self) -> R<u64> {
        let s = self.take(8)?;
        Ok(u64::from_le_bytes(s.try_into().expect("8 bytes")))
    }

    fn skip(&mut self, wire: u8) -> R<()> {
        match wire {
            0 => self.varint().map(drop),
            1 => self.take(8).map(drop),
            2 => self.bytes().map(drop),
            5 => self.take(4).map(drop),
            _ => Err(format!("unsupported wire type {wire}")),
        }
    }
}

fn utf8(b: &[u8]) -> R<String> {
    String::from_utf8(b.to_vec()).map_err(|_| "a string isn't UTF-8".into())
}

fn any_value(buf: &[u8], depth: usize) -> R<Value> {
    if depth > MAX_DEPTH {
        return Err("values nested too deep".into());
    }
    let mut r = Reader::new(buf);
    let mut out = Value::Null;
    while !r.done() {
        let (field, wire) = r.key()?;
        out = match (field, wire) {
            (1, 2) => Value::String(utf8(r.bytes()?)?),
            (2, 0) => Value::Bool(r.varint()? != 0),
            (3, 0) => Value::from(r.varint()? as i64),
            (4, 1) => Value::from(f64::from_bits(r.fixed64()?)),
            (5, 2) => {
                let inner = r.bytes()?;
                let mut a = Reader::new(inner);
                let mut items = Vec::new();
                while !a.done() {
                    let (f, w) = a.key()?;
                    if (f, w) == (1, 2) {
                        items.push(any_value(a.bytes()?, depth + 1)?);
                    } else {
                        a.skip(w)?;
                    }
                }
                Value::Array(items)
            }
            (6, 2) => Value::Object(kv_list(r.bytes()?, depth + 1)?.into_iter().collect()),
            // bytes_value: not text; kept as nothing.
            (7, 2) => {
                r.bytes()?;
                Value::Null
            }
            (_, w) => {
                r.skip(w)?;
                out
            }
        };
    }
    Ok(out)
}

fn key_value(buf: &[u8], depth: usize) -> R<(String, Value)> {
    let mut r = Reader::new(buf);
    let mut key = String::new();
    let mut value = Value::Null;
    while !r.done() {
        match r.key()? {
            (1, 2) => key = utf8(r.bytes()?)?,
            (2, 2) => value = any_value(r.bytes()?, depth)?,
            (_, w) => r.skip(w)?,
        }
    }
    Ok((key, value))
}

fn kv_list(buf: &[u8], depth: usize) -> R<BTreeMap<String, Value>> {
    let mut r = Reader::new(buf);
    let mut out = BTreeMap::new();
    while !r.done() {
        match r.key()? {
            (1, 2) => {
                let (k, v) = key_value(r.bytes()?, depth)?;
                out.insert(k, v);
            }
            (_, w) => r.skip(w)?,
        }
    }
    Ok(out)
}

fn resource(buf: &[u8]) -> R<BTreeMap<String, Value>> {
    let mut r = Reader::new(buf);
    let mut out = BTreeMap::new();
    while !r.done() {
        match r.key()? {
            (1, 2) => {
                let (k, v) = key_value(r.bytes()?, 0)?;
                out.insert(k, v);
            }
            (_, w) => r.skip(w)?,
        }
    }
    Ok(out)
}

fn log_record(buf: &[u8], res: &BTreeMap<String, Value>) -> R<LogRecord> {
    let mut r = Reader::new(buf);
    let mut out = LogRecord {
        resource: res.clone(),
        ..LogRecord::default()
    };
    let mut observed = 0;
    while !r.done() {
        match r.key()? {
            (1, 1) => out.time_unix_nano = r.fixed64()?,
            (11, 1) => observed = r.fixed64()?,
            (5, 2) => out.body = Some(any_value(r.bytes()?, 0)?),
            (6, 2) => {
                let (k, v) = key_value(r.bytes()?, 0)?;
                out.attributes.insert(k, v);
            }
            (12, 2) => out.event_name = Some(utf8(r.bytes()?)?),
            (_, w) => r.skip(w)?,
        }
    }
    if out.time_unix_nano == 0 {
        out.time_unix_nano = observed;
    }
    Ok(out)
}

/// Every log record of an `ExportLogsServiceRequest`, at most `max`.
pub fn decode(buf: &[u8], max: usize) -> R<Vec<LogRecord>> {
    let mut out = Vec::new();
    let mut top = Reader::new(buf);
    while !top.done() {
        let (field, wire) = top.key()?;
        if (field, wire) != (1, 2) {
            top.skip(wire)?;
            continue;
        }
        // ResourceLogs
        let mut rl = Reader::new(top.bytes()?);
        let mut res = BTreeMap::new();
        let mut scopes: Vec<&[u8]> = Vec::new();
        while !rl.done() {
            match rl.key()? {
                (1, 2) => res = resource(rl.bytes()?)?,
                (2, 2) => scopes.push(rl.bytes()?),
                (_, w) => rl.skip(w)?,
            }
        }
        for scope in scopes {
            let mut sl = Reader::new(scope);
            while !sl.done() {
                match sl.key()? {
                    (2, 2) => {
                        out.push(log_record(sl.bytes()?, &res)?);
                        if out.len() > max {
                            return Err(format!("more than {max} records in one request"));
                        }
                    }
                    (_, w) => sl.skip(w)?,
                }
            }
        }
    }
    Ok(out)
}

// The encoder (tests: the stand-in stream).

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn enc_any(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    match v {
        Value::String(s) => put_bytes(&mut out, 1, s.as_bytes()),
        Value::Bool(b) => {
            put_varint(&mut out, 2 << 3);
            put_varint(&mut out, u64::from(*b));
        }
        Value::Number(n) if n.is_i64() || n.is_u64() => {
            put_varint(&mut out, 3 << 3);
            put_varint(&mut out, n.as_i64().unwrap_or_default() as u64);
        }
        Value::Number(n) => {
            put_varint(&mut out, (4 << 3) | 1);
            out.extend_from_slice(&n.as_f64().unwrap_or_default().to_bits().to_le_bytes());
        }
        Value::Array(items) => {
            let mut a = Vec::new();
            for i in items {
                put_bytes(&mut a, 1, &enc_any(i));
            }
            put_bytes(&mut out, 5, &a);
        }
        Value::Object(map) => put_bytes(&mut out, 6, &enc_kvs(map)),
        Value::Null => {}
    }
    out
}

fn enc_kv(k: &str, v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, 1, k.as_bytes());
    put_bytes(&mut out, 2, &enc_any(v));
    out
}

fn enc_kvs(map: &Map<String, Value>) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in map {
        put_bytes(&mut out, 1, &enc_kv(k, v));
    }
    out
}

/// An `ExportLogsServiceRequest` holding `records` under one resource with `resource` attributes.
pub fn encode(records: &[LogRecord], resource: &BTreeMap<String, Value>) -> Vec<u8> {
    let mut res = Vec::new();
    for (k, v) in resource {
        put_bytes(&mut res, 1, &enc_kv(k, v));
    }
    let mut scope = Vec::new();
    for r in records {
        let mut rec = Vec::new();
        put_varint(&mut rec, (1 << 3) | 1);
        rec.extend_from_slice(&r.time_unix_nano.to_le_bytes());
        if let Some(b) = &r.body {
            put_bytes(&mut rec, 5, &enc_any(b));
        }
        for (k, v) in &r.attributes {
            put_bytes(&mut rec, 6, &enc_kv(k, v));
        }
        if let Some(n) = &r.event_name {
            put_bytes(&mut rec, 12, n.as_bytes());
        }
        put_bytes(&mut scope, 2, &rec);
    }
    let mut rl = Vec::new();
    put_bytes(&mut rl, 1, &res);
    put_bytes(&mut rl, 2, &scope);
    let mut out = Vec::new();
    put_bytes(&mut out, 1, &rl);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_is_encoded_decodes_the_same() {
        let rec = LogRecord {
            time_unix_nano: 1_764_000_000_000_000_000,
            event_name: Some("openshell.ocsf".into()),
            body: Some(json!("{\"class_uid\":4001}")),
            attributes: [
                ("ocsf.metadata.uid".to_string(), json!("u-1")),
                ("n".to_string(), json!(42)),
                ("list".to_string(), json!([true, 1.5, {"k": "v"}])),
            ]
            .into_iter()
            .collect(),
            resource: Default::default(),
        };
        let res: BTreeMap<String, Value> =
            [("openshell.sandbox.id".to_string(), json!("sb-1"))].into();
        let bytes = encode(&[rec.clone(), rec.clone()], &res);
        let back = decode(&bytes, 10).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].attributes, rec.attributes);
        assert_eq!(back[0].body, rec.body);
        assert_eq!(back[0].event_name, rec.event_name);
        assert_eq!(back[0].resource["openshell.sandbox.id"], "sb-1");
        assert!(decode(&bytes, 1).is_err(), "the record cap");
    }

    #[test]
    fn malformed_input_is_an_error() {
        for bad in [&[0x0a, 0xff][..], &[0x0a, 0x05, 0x01], &[0x0b], &[0xff; 11]] {
            assert!(decode(bad, 10).is_err(), "{bad:?}");
        }
        assert_eq!(decode(&[], 10).unwrap(), Vec::new());
        // Deep nesting is refused, not followed.
        let mut v = json!("x");
        for _ in 0..40 {
            v = json!([v]);
        }
        let rec = LogRecord {
            body: Some(v),
            ..LogRecord::default()
        };
        assert!(decode(&encode(&[rec], &BTreeMap::new()), 10).is_err());
    }
}
