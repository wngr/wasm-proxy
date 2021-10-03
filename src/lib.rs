use std::{convert::TryInto, sync::Arc};

use futures::{
    channel::{mpsc, oneshot},
    StreamExt,
};
use js_sys::{Object, Proxy, Reflect};
use log::*;
use parking_lot::Mutex;
use serde_json::Value;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;
use wasm_futures_executor::ThreadPool;

#[wasm_bindgen(start)]
pub fn start() {
    let _ = console_log::init_with_level(log::Level::Debug);
    ::console_error_panic_hook::set_once();
    debug!("Setup logging");
}

type DocVersion = u64;
type TransactionId = u64;
enum DocCommand {
    StartTransaction {
        tx: oneshot::Sender<TransactionId>,
    },
    GetValue {
        //id: TransactionId,
        pointer: String,
        tx: oneshot::Sender<Option<serde_json::Value>>,
    },
    SetValue {
        id: TransactionId,
        pointer: String,
        val: Option<serde_json::Value>,
        //tx: oneshot::Sender<>,
    },
    EndTransaction {
        id: TransactionId,
    },
}

#[wasm_bindgen]
struct Document {
    // TODO: magically update `version`
    version: DocVersion,
    tx: mpsc::Sender<DocCommand>,
    pool: ThreadPool,
}

#[wasm_bindgen]
impl Document {
    pub async fn init() -> Result<Document, JsValue> {
        let (tx, mut rx) = mpsc::channel(1);
        let pool = ThreadPool::new(1).await?;
        pool.spawn_ok(async move {
            let mut state = serde_json::Value::Object(Default::default());
            let mut command_queue = vec![];
            let mut current_transaction = None;
            let mut next_tx = 0u64;
            while let Some(cmd) = rx.next().await {
                match cmd {
                    c @ DocCommand::StartTransaction { .. } if current_transaction.is_some() => {
                        command_queue.push(c);
                    }
                    DocCommand::StartTransaction { tx } if current_transaction.is_none() => {
                        current_transaction.replace(next_tx);
                        tx.send(next_tx);
                    }
                    DocCommand::GetValue { pointer, tx } => {
                        tx.send(state.pointer(&pointer).cloned());
                    }
                    DocCommand::SetValue { id, pointer, val }
                        if Some(&id) == current_transaction.as_ref() =>
                    {
                        if let Some(s) = state.get_mut(&pointer) {
                            val.expect("TODO Handle deletion");
                        }
                    }
                    c @ DocCommand::SetValue { .. } => {
                        command_queue.push(c);
                    }
                    DocCommand::EndTransaction { id } => {
                        next_tx += 1;
                        current_transaction.take();
                    }
                }
            }
        });
        Ok(Self {
            version: 0,
            pool,
            tx,
        })
    }
    // TODO: add manual ts types for `f`
    pub fn change(&self, f: &js_sys::Function) -> Result<(), JsValue> {
        let tx_c = self.tx.clone();
        let proxy_get = Closure::wrap(Box::new(move |obj: JsValue, prop: JsValue| {
            info!("{:?} {:?} ", obj, prop);
            let val = if let Some(s) = prop.as_string() {
                state.get(&s)
            } else if let Some(u) = prop.as_f64() {
                todo!("support number indices");
            } else {
                let idx = prop.as_bool().unwrap().to_string();
                state.get(&idx)
            };
            if let Some(x) = val {
                Ok(match x {
                    Value::Null => JsValue::null(),
                    Value::Bool(b) => JsValue::from_bool(*b),
                    Value::Number(n) => JsValue::from_f64(n.as_f64().unwrap()),
                    Value::String(s) => JsValue::from_str(&s),
                    Value::Array(_) => todo!(),
                    Value::Object(_) => todo!(),
                })
            } else {
                Ok(JsValue::undefined())
            }
        })
            as Box<dyn Fn(JsValue, JsValue) -> Result<JsValue, JsValue>>);
        let changes_c = changes.clone();
        let proxy_set = Closure::wrap(Box::new(
            move |obj: JsValue, prop: JsValue, value: JsValue| {
                info!("{:?} {:?} {:?}", obj, prop, value);
                if value.is_object() {
                    // We' need to proxy `value` again in order to be called again on nested changes.
                    return Err("Manipulating nested objects is not yet supported".into());
                }
                let path = prop.as_string().unwrap();
                changes_c.lock().push((path, value.clone()));
                Reflect::set(&obj, &prop, &value)
            },
        )
            as Box<dyn Fn(JsValue, JsValue, JsValue) -> Result<bool, JsValue>>);
        let handler = Object::new();
        Reflect::set(&handler, &"get".into(), proxy_get.as_ref())?;
        Reflect::set(&handler, &"set".into(), proxy_set.as_ref())?;
        let target = Object::new();
        let proxy = Proxy::new(&target, &handler);

        // Lock current state
        let mut state = self.state.lock();
        {
            let mut obj = state.as_object_mut().unwrap();
            f.call1(&JsValue::null(), &proxy)?;
            info!("Changes: {:?}", changes);
            for (path, prop) in changes.lock().drain(..) {
                let p = if let Some(s) = prop.as_string() {
                    Value::String(s)
                } else if let Some(x) = prop.as_bool() {
                    Value::Bool(x)
                } else if let Some(x) = prop.as_f64() {
                    Value::Number(serde_json::value::Number::from_f64(x).unwrap())
                } else {
                    unreachable!()
                };
                obj.insert(path, p);
            }
        }
        info!("Result: {:?}", state);
        Ok(())
    }
}
