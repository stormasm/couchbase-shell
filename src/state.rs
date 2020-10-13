use couchbase::{Bucket, Cluster};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

pub struct State {
    active: Mutex<String>,
    clusters: HashMap<String, RemoteCluster>,
}

impl State {
    pub fn new(clusters: HashMap<String, RemoteCluster>, active: String) -> Self {
        let state = Self {
            active: Mutex::new(active.clone()),
            clusters,
        };
        state.set_active(active).unwrap();
        state
    }

    pub fn clusters(&self) -> &HashMap<String, RemoteCluster> {
        &self.clusters
    }

    pub fn active(&self) -> String {
        self.active.lock().unwrap().clone()
    }

    pub fn set_active(&self, active: String) -> Result<(), u32> {
        if !self.clusters.contains_key(&active) {
            return Err(1); // make me proper!
        }

        {
            let mut guard = self.active.lock().unwrap();
            *guard = active.clone();
        }

        let remote = self.active_cluster();
        let _ = remote.cluster();

        if remote.active_bucket().is_some() {
            let _ = remote.bucket(remote.active_bucket().unwrap().as_str());
        }

        for (k, v) in &self.clusters {
            if k != &active {
                v.deactivate()
            }
        }

        Ok(())
    }

    pub fn active_cluster(&self) -> &RemoteCluster {
        let active = self.active.lock().unwrap();
        &self
            .clusters
            .get(&*active)
            .expect("No active cluster, this is a bug :(")
    }
}

pub struct RemoteCluster {
    connstr: String,
    username: String,
    password: String,
    cluster: Mutex<Option<Arc<Cluster>>>,
    buckets: Mutex<HashMap<String, Arc<Bucket>>>,
    active_bucket: Mutex<Option<String>>,
}

impl RemoteCluster {
    pub fn new(
        connstr: String,
        username: String,
        password: String,
        active_bucket: Option<String>,
    ) -> Self {
        Self {
            cluster: Mutex::new(None),
            buckets: Mutex::new(HashMap::new()),
            connstr,
            username,
            password,
            active_bucket: Mutex::new(active_bucket),
        }
    }

    pub fn cluster(&self) -> Arc<Cluster> {
        let mut c = self.cluster.lock().unwrap();
        if c.is_none() {
            *c = Some(Arc::new(Cluster::connect(
                &self.connstr,
                &self.username,
                &self.password,
            )));
        }
        c.as_ref().unwrap().clone()
    }

    pub fn bucket(&self, name: &str) -> Arc<Bucket> {
        let mut buckets = self.buckets.lock().unwrap();
        if !buckets.contains_key(name) {
            let bucket = self.cluster().bucket(name);
            buckets.insert(name.into(), Arc::new(bucket));
        }
        buckets.get(name).unwrap().clone()
    }

    pub fn active_bucket(&self) -> Option<String> {
        self.active_bucket
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.clone())
    }

    pub fn set_active_bucket(&self, name: String) {
        let mut active = self.active_bucket.lock().unwrap();
        *active = Some(name);
    }

    pub fn deactivate(&self) {
        let mut c = self.cluster.lock().unwrap();
        if c.is_some() {
            *c = None;
        }
    }

    pub fn username(&self) -> &str {
        self.username.as_str()
    }

    pub fn password(&self) -> &str {
        self.password.as_str()
    }

    pub fn connstr(&self) -> &str {
        self.connstr.as_str()
    }
}