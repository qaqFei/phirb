mod chart;
pub use chart::*;

mod record;
pub use record::*;

mod user;
pub use user::*;

use super::Client;
use crate::{
    dir,
    images::{THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH},
};
use anyhow::Result;
use bytes::Bytes;
use futures_util::Stream;
use http_cache_reqwest::{CACacheManager, Cache, CacheMode, HttpCache};
use image::DynamicImage;
use lru::LruCache;
use once_cell::sync::Lazy;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use serde::{de::DeserializeOwned, Deserialize, Serialize, Serializer};
use std::{
    any::Any,
    collections::HashMap,
    marker::PhantomData,
    sync::{Arc, Mutex},
};

pub(crate) type ObjectMap<T> = LruCache<i32, Arc<T>>;
static CACHES: Lazy<Mutex<HashMap<&'static str, Arc<Mutex<Box<dyn Any + Send + Sync>>>>>> = Lazy::new(Mutex::default);

pub(crate) fn obtain_map_cache<T: PZObject + 'static>() -> Arc<Mutex<Box<dyn Any + Send + Sync>>> {
    let mut caches = CACHES.lock().unwrap();
    Arc::clone(
        caches
            .entry(T::QUERY_PATH)
            .or_insert_with(|| Arc::new(Mutex::new(Box::new(ObjectMap::<T>::new(64.try_into().unwrap()))))),
    )
}

pub trait PZObject: Clone + DeserializeOwned + Send + Sync {
    const QUERY_PATH: &'static str;

    fn id(&self) -> i32;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String")]
#[serde(into = "String")]
pub struct MusicPosition {
    pub seconds: u32,
}
impl TryFrom<String> for MusicPosition {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let seconds = || -> Option<u32> {
            let mut it = value.splitn(3, ':');
            let mut res = it.next()?.parse::<u32>().ok()?;
            res = res * 60 + it.next()?.parse::<u32>().ok()?;
            res = res * 60 + it.next()?.parse::<u32>().ok()?;
            Some(res)
        }()
        .ok_or("illegal position")?;
        Ok(MusicPosition { seconds })
    }
}
impl From<MusicPosition> for String {
    fn from(value: MusicPosition) -> Self {
        format!("00:00:{:02}", value.seconds)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(try_from = "u8")]
#[repr(u8)]
pub enum LevelType {
    EZ = 0,
    HD,
    IN,
    AT,
    SP,
}
impl TryFrom<u8> for LevelType {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        use LevelType::*;
        Ok(match value {
            0 => EZ,
            1 => HD,
            2 => IN,
            3 => AT,
            4 => SP,
            x => {
                return Err(format!("illegal level type: {x}"));
            }
        })
    }
}

#[derive(Debug)]
pub struct Ptr<T> {
    id: i32,
    _marker: PhantomData<T>,
}
impl<T: PZObject + 'static> Clone for Ptr<T> {
    fn clone(&self) -> Self {
        Self::new(self.id)
    }
}
impl<T: PZObject + 'static> From<i32> for Ptr<T> {
    fn from(value: i32) -> Self {
        Self::new(value)
    }
}

impl<T: PZObject + 'static> Ptr<T> {
    pub fn new(id: i32) -> Self {
        Self {
            id,
            _marker: PhantomData::default(),
        }
    }

    #[inline]
    pub async fn fetch(&self) -> Result<Arc<T>> {
        Client::fetch(self.id).await
    }

    pub async fn load(&self) -> Result<Arc<T>> {
        // sync locks can not be held accross await point
        {
            let map = obtain_map_cache::<T>();
            let mut guard = map.lock().unwrap();
            let Some(actual_map) = guard.downcast_mut::<ObjectMap::<T>>() else { unreachable!() };
            if let Some(value) = actual_map.get(&self.id) {
                return Ok(Arc::clone(value));
            }
            drop(guard);
            drop(map);
        }
        self.fetch().await
    }
}
impl<T: PZObject + 'static> Serialize for Ptr<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(self.id)
    }
}
impl<'de, T: PZObject + 'static> Deserialize<'de> for Ptr<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        i32::deserialize(deserializer).map(Self::new)
    }
}

pub static CACHE_CLIENT: Lazy<ClientWithMiddleware> = Lazy::new(|| {
    ClientBuilder::new(reqwest::Client::new())
        .with(Cache(HttpCache {
            mode: CacheMode::Default,
            manager: CACacheManager {
                path: format!("{}/http-cache", dir::cache().unwrap_or_else(|_| ".".to_owned())),
            },
            options: None,
        }))
        .build()
});

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PZFile {
    pub url: String,
}
impl PZFile {
    pub async fn fetch(&self) -> Result<Bytes> {
        Ok(CACHE_CLIENT.get(&self.url).send().await?.bytes().await?)
    }

    pub async fn fetch_stream(&self) -> Result<impl Stream<Item = reqwest::Result<Bytes>>> {
        Ok(CACHE_CLIENT.get(&self.url).send().await?.bytes_stream())
    }

    pub async fn load_image(&self) -> Result<DynamicImage> {
        Ok(image::load_from_memory(&self.fetch().await?)?)
    }

    pub async fn load_thumbnail(&self) -> Result<DynamicImage> {
        if self.url.starts_with("https://phira.mivik.cn/") {
            return PZFile {
                url: format!("{}?imageView/0/w/{THUMBNAIL_WIDTH}/h/{THUMBNAIL_HEIGHT}", self.url),
            }
            .load_image()
            .await;
        }
        self.load_image().await
    }
}
