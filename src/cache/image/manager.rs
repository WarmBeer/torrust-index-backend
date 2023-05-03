use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use tokio::sync::RwLock;

use crate::cache::cache::BytesCache;
use crate::config::Configuration;
use crate::models::user::UserCompact;

pub enum Error {
    UrlIsUnreachable,
    UrlIsNotAnImage,
    ImageTooBig,
    UserQuotaMet,
    Unauthenticated,
}

type UserQuotas = HashMap<i64, ImageCacheQuota>;

pub fn now_in_secs() -> u64 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(n) => n.as_secs(),
        Err(_) => panic!("SystemTime before UNIX EPOCH!"),
    }
}

#[derive(Clone)]
pub struct ImageCacheQuota {
    pub user_id: i64,
    pub usage: usize,
    pub max_usage: usize,
    pub date_start_secs: u64,
    pub period_secs: u64,
}

impl ImageCacheQuota {
    pub fn new(user_id: i64, max_usage: usize, period_secs: u64) -> Self {
        Self {
            user_id,
            usage: 0,
            max_usage,
            date_start_secs: now_in_secs(),
            period_secs,
        }
    }

    pub fn add_usage(&mut self, amount: usize) -> Result<(), ()> {
        // Check if quota needs to be reset.
        if now_in_secs() - self.date_start_secs > self.period_secs {
            self.reset();
        }

        if self.is_reached() {
            return Err(());
        }

        self.usage = self.usage.saturating_add(amount);

        Ok(())
    }

    pub fn reset(&mut self) {
        self.usage = 0;
        self.date_start_secs = now_in_secs();
    }

    pub fn is_reached(&self) -> bool {
        self.usage >= self.max_usage
    }
}

pub struct ImageCacheService {
    image_cache: RwLock<BytesCache>,
    user_quotas: RwLock<UserQuotas>,
    reqwest_client: reqwest::Client,
    cfg: Arc<Configuration>,
}

impl ImageCacheService {
    pub async fn new(cfg: Arc<Configuration>) -> Self {
        let settings = cfg.settings.read().await;

        let image_cache =
            BytesCache::with_capacity_and_entry_size_limit(settings.image_cache.capacity, settings.image_cache.entry_size_limit)
                .expect("Could not create image cache.");

        let reqwest_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(settings.image_cache.max_request_timeout_ms))
            .build()
            .unwrap();

        drop(settings);

        Self {
            image_cache: RwLock::new(image_cache),
            user_quotas: RwLock::new(HashMap::new()),
            reqwest_client,
            cfg,
        }
    }

    /// Get an image from the url and insert it into the cache if it isn't cached already.
    /// Unauthenticated users can only get already cached images.
    pub async fn get_image_by_url(&self, url: &str, opt_user: Option<UserCompact>) -> Result<Bytes, Error> {
        if let Some(entry) = self.image_cache.read().await.get(url).await {
            return Ok(entry.bytes);
        }

        if opt_user.is_none() {
            return Err(Error::Unauthenticated);
        }

        let user = opt_user.unwrap();

        self.check_user_quota(&user).await?;

        let image_bytes = self.get_image_from_url_as_bytes(url).await?;

        self.check_image_size(&image_bytes).await?;

        // These two functions could be executed after returning the image to the client,
        // but than we would need a dedicated task or thread that executes these functions.
        // This can be problematic if a task is spawned after every user request.
        // Since these functions execute very fast, I don't see a reason to further optimize this.
        // For now.
        self.update_image_cache(url, &image_bytes).await?;

        self.update_user_quota(&user, image_bytes.len()).await?;

        Ok(image_bytes)
    }

    async fn get_image_from_url_as_bytes(&self, url: &str) -> Result<Bytes, Error> {
        let res = self
            .reqwest_client
            .clone()
            .get(url)
            .send()
            .await
            .map_err(|_| Error::UrlIsUnreachable)?;

        if let Some(content_type) = res.headers().get("Content-Type") {
            if content_type != "image/jpeg" && content_type != "image/png" {
                return Err(Error::UrlIsNotAnImage);
            }
        } else {
            return Err(Error::UrlIsNotAnImage);
        }

        res.bytes().await.map_err(|_| Error::UrlIsNotAnImage)
    }

    async fn check_user_quota(&self, user: &UserCompact) -> Result<(), Error> {
        if let Some(quota) = self.user_quotas.read().await.get(&user.user_id) {
            if quota.is_reached() {
                return Err(Error::UserQuotaMet);
            }
        }

        Ok(())
    }

    async fn check_image_size(&self, image_bytes: &Bytes) -> Result<(), Error> {
        let settings = self.cfg.settings.read().await;

        if image_bytes.len() > settings.image_cache.entry_size_limit {
            return Err(Error::ImageTooBig);
        }

        Ok(())
    }

    async fn update_image_cache(&self, url: &str, image_bytes: &Bytes) -> Result<(), Error> {
        if self
            .image_cache
            .write()
            .await
            .set(url.to_string(), image_bytes.clone())
            .await
            .is_err()
        {
            return Err(Error::ImageTooBig);
        }

        Ok(())
    }

    async fn update_user_quota(&self, user: &UserCompact, amount: usize) -> Result<(), Error> {
        let settings = self.cfg.settings.read().await;

        let mut quota = self
            .user_quotas
            .read()
            .await
            .get(&user.user_id)
            .cloned()
            .unwrap_or(ImageCacheQuota::new(
                user.user_id,
                settings.image_cache.user_quota_bytes,
                settings.image_cache.user_quota_period_seconds,
            ));

        let _ = quota.add_usage(amount);

        let _ = self.user_quotas.write().await.insert(user.user_id, quota);

        Ok(())
    }
}
