use std::collections::HashMap;

use pipewire::proxy::Listener;

#[derive(Default)]
pub struct Proxies {
    pub listeners: HashMap<u32, Vec<Box<dyn Listener>>>,
}

impl Proxies {
    pub fn add_listener<L: Listener + 'static>(&mut self, proxy_id: u32, listener: L) {
        let v = self.listeners.entry(proxy_id).or_default();
        v.push(Box::new(listener));
    }

    pub fn remove_proxy(&mut self, proxy_id: u32) {
        self.listeners.remove(&proxy_id);
    }
}
