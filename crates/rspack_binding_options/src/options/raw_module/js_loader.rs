use std::{
  path::{Path, PathBuf},
  str::FromStr,
};

use napi_derive::napi;
use rspack_core::{rspack_sources::SourceMap, Content, ResourceData};
use rspack_error::Diagnostic;
use rustc_hash::FxHashSet as HashSet;
use tracing::{span_enabled, Level};
use {
  napi::bindgen_prelude::*,
  napi::NapiRaw,
  rspack_binding_macros::call_js_function_with_napi_objects,
  rspack_core::{Loader, LoaderContext, LoaderRunnerContext},
  rspack_error::internal_error,
  rspack_identifier::{Identifiable, Identifier},
  rspack_napi_shared::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode},
  rspack_napi_shared::{NapiResultExt, NAPI_ENV},
};

use crate::get_builtin_loader;

/// Loader Runner for JavaScript environment
#[derive(Clone)]
pub enum JsLoaderRunner {
  ThreadsafeFunction(ThreadsafeFunction<JsLoaderContext, LoaderThreadsafeLoaderResult>),
  /// Used for non-JavaScript environment such as calling from the Rust side for testing purposes
  Noop,
}

impl JsLoaderRunner {
  pub fn noop() -> Self {
    Self::Noop
  }

  pub fn call(
    &self,
    value: JsLoaderContext,
    mode: ThreadsafeFunctionCallMode,
  ) -> Result<tokio::sync::oneshot::Receiver<rspack_error::Result<LoaderThreadsafeLoaderResult>>>
  {
    match self {
      Self::ThreadsafeFunction(func) => func.call(value, mode),
      Self::Noop => unreachable!(),
    }
  }
}

impl TryFrom<JsFunction> for JsLoaderRunner {
  type Error = napi::Error;

  fn try_from(value: JsFunction) -> std::result::Result<Self, Self::Error> {
    let loader_runner = unsafe { value.raw() };

    let func = NAPI_ENV.with(|env| -> anyhow::Result<_> {
      let env = env
        .borrow()
        .expect("Failed to get env, did you forget to call it from node?");

      let mut func = ThreadsafeFunction::<JsLoaderContext, LoaderThreadsafeLoaderResult>::create(
        env,
        loader_runner,
        0,
        |ctx| {
          let (ctx, resolver) = ctx.split_into_parts();

          let env = ctx.env;
          let cb = ctx.callback;
          let resource = ctx.value.resource.clone();

          let loader_name = if span_enabled!(Level::TRACE) {
            let loader_path = &ctx.value.current_loader;
            // try to remove the previous node_modules parts from path for better display

            let parts = loader_path.split("node_modules/");
            let loader_name: &str = parts.last().unwrap_or(loader_path.as_str());
            String::from(loader_name)
          } else {
            String::from("unknown")
          };
          let result = tracing::span!(
            tracing::Level::INFO,
            "loader_sync_call",
            resource = &resource,
            loader_name = &loader_name
          )
          .in_scope(|| unsafe { call_js_function_with_napi_objects!(env, cb, ctx.value) });

          let resolve_start = std::time::Instant::now();
          resolver.resolve::<Option<JsLoaderResult>>(result, move |_, r| {
            tracing::trace!(
              "Finish resolving loader result for {}, took {}ms",
              resource,
              resolve_start.elapsed().as_millis()
            );
            Ok(r)
          })
        },
      )?;
      func.unref(&Env::from(env))?;
      Ok(func)
    })?;

    Ok(Self::ThreadsafeFunction(func))
  }
}

pub struct JsLoaderAdapter {
  pub runner: JsLoaderRunner,
  pub identifier: Identifier,
}

impl std::fmt::Debug for JsLoaderAdapter {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("JsLoaderAdapter")
      .field("loaders", &self.identifier)
      .finish()
  }
}

impl Identifiable for JsLoaderAdapter {
  fn identifier(&self) -> Identifier {
    self.identifier
  }
}

#[async_trait::async_trait]
impl Loader<LoaderRunnerContext> for JsLoaderAdapter {
  async fn pitch(
    &self,
    loader_context: &mut LoaderContext<'_, LoaderRunnerContext>,
  ) -> rspack_error::Result<()> {
    let mut js_loader_context: JsLoaderContext = (&*loader_context).try_into()?;
    js_loader_context.is_pitching = true;

    let loader_result = self
      .runner
      .call(js_loader_context, ThreadsafeFunctionCallMode::NonBlocking)
      .into_rspack_result()?
      .await
      .map_err(|err| internal_error!("Failed to call loader: {err}"))??;

    if let Some(loader_result) = loader_result {
      // This indicate that the JS loaders pitched(return something) successfully
      // and executed the normal loader on the JS loader side(in that group),
      // then here we want to change the control flow in order
      // to execute the remaining normal loaders on the native side.
      if !loader_result.is_pitching {
        loader_context
          .current_loader()
          .__do_not_use_or_you_will_be_fired_set_normal_executed();
      }
      sync_loader_context(loader_result, loader_context)?;
    }

    Ok(())
  }
  async fn run(
    &self,
    loader_context: &mut LoaderContext<'_, LoaderRunnerContext>,
  ) -> rspack_error::Result<()> {
    let mut js_loader_context: JsLoaderContext = (&*loader_context).try_into()?;
    // Instruct the JS loader-runner to execute loaders in backwards.
    js_loader_context.is_pitching = false;

    let loader_result = self
      .runner
      .call(js_loader_context, ThreadsafeFunctionCallMode::NonBlocking)
      .into_rspack_result()?
      .await
      .map_err(|err| internal_error!("Failed to call loader: {err}"))??;

    if let Some(loader_result) = loader_result {
      sync_loader_context(loader_result, loader_context)?;
    }

    Ok(())
  }
}

fn sync_loader_context(
  loader_result: JsLoaderResult,
  loader_context: &mut LoaderContext<'_, LoaderRunnerContext>,
) -> rspack_error::Result<()> {
  loader_context.cacheable = loader_result.cacheable;
  loader_context.file_dependencies = loader_result
    .file_dependencies
    .into_iter()
    .map(std::path::PathBuf::from)
    .collect();
  loader_context.context_dependencies = loader_result
    .context_dependencies
    .into_iter()
    .map(std::path::PathBuf::from)
    .collect();
  loader_context.missing_dependencies = loader_result
    .missing_dependencies
    .into_iter()
    .map(std::path::PathBuf::from)
    .collect();
  loader_context.build_dependencies = loader_result
    .build_dependencies
    .into_iter()
    .map(std::path::PathBuf::from)
    .collect();
  loader_context.content = loader_result
    .content
    .map(|c| rspack_core::Content::from(Into::<Vec<u8>>::into(c)));
  loader_context.source_map = loader_result
    .source_map
    .as_ref()
    .map(|s| rspack_core::rspack_sources::SourceMap::from_slice(s))
    .transpose()
    .map_err(|e| internal_error!(e.to_string()))?;
  loader_context.additional_data = loader_result
    .additional_data
    .map(|item| String::from_utf8_lossy(&item).to_string());

  Ok(())
}

#[napi(object)]
pub struct JsLoaderContext {
  /// Content maybe empty in pitching stage
  pub content: Option<Buffer>,
  pub additional_data: Option<Buffer>,
  pub source_map: Option<Buffer>,
  pub resource: String,
  pub resource_path: String,
  pub resource_query: Option<String>,
  pub resource_fragment: Option<String>,
  pub cacheable: bool,
  pub file_dependencies: Vec<String>,
  pub context_dependencies: Vec<String>,
  pub missing_dependencies: Vec<String>,
  pub build_dependencies: Vec<String>,
  pub asset_filenames: Vec<String>,

  pub current_loader: String,
  pub is_pitching: bool,
  /// Internal loader context
  /// @internal
  pub context: External<rspack_core::LoaderRunnerContext>,
  /// Internal loader diagnostic
  /// @internal
  pub diagnostics: External<Vec<Diagnostic>>,
}

impl TryFrom<&rspack_core::LoaderContext<'_, rspack_core::LoaderRunnerContext>>
  for JsLoaderContext
{
  type Error = rspack_error::Error;

  fn try_from(
    cx: &rspack_core::LoaderContext<'_, rspack_core::LoaderRunnerContext>,
  ) -> std::result::Result<Self, Self::Error> {
    Ok(JsLoaderContext {
      content: cx
        .content
        .as_ref()
        .map(|c| c.to_owned().into_bytes().into()),
      additional_data: cx.additional_data.to_owned().map(|v| v.into_bytes().into()),
      source_map: cx
        .source_map
        .clone()
        .map(|v| v.to_json())
        .transpose()
        .map_err(|e| internal_error!(e.to_string()))?
        .map(|v| v.into_bytes().into()),
      resource: cx.resource.to_owned(),
      resource_path: cx.resource_path.to_string_lossy().to_string(),
      resource_fragment: cx.resource_fragment.map(|r| r.to_owned()),
      resource_query: cx.resource_query.map(|r| r.to_owned()),
      cacheable: cx.cacheable,
      file_dependencies: cx
        .file_dependencies
        .iter()
        .map(|i| i.to_string_lossy().to_string())
        .collect(),
      context_dependencies: cx
        .context_dependencies
        .iter()
        .map(|i| i.to_string_lossy().to_string())
        .collect(),
      missing_dependencies: cx
        .missing_dependencies
        .iter()
        .map(|i| i.to_string_lossy().to_string())
        .collect(),
      build_dependencies: cx
        .build_dependencies
        .iter()
        .map(|i| i.to_string_lossy().to_string())
        .collect(),
      asset_filenames: cx.asset_filenames.iter().map(|i| i.to_owned()).collect(),

      current_loader: cx.current_loader().to_string(),
      is_pitching: true,
      context: External::new(cx.context.clone()),
      diagnostics: External::new(cx.diagnostics.clone()),
    })
  }
}

pub async fn run_builtin_loader(
  builtin: String,
  options: Option<&str>,
  loader_context: JsLoaderContext,
) -> Result<JsLoaderContext> {
  use rspack_loader_runner::__private::loader::LoaderItemList;

  let loader = get_builtin_loader(&builtin, options);
  let loader_item = loader.clone().into();
  let list = &[loader_item];

  let mut cx = LoaderContext {
    content: loader_context
      .content
      .map(|c| Content::from(c.as_ref().to_owned())),
    resource: &loader_context.resource,
    resource_path: Path::new(&loader_context.resource_path),
    resource_query: loader_context.resource_query.as_deref(),
    resource_fragment: loader_context.resource_fragment.as_deref(),
    context: loader_context.context.clone(),
    source_map: loader_context
      .source_map
      .map(|s| SourceMap::from_slice(s.as_ref()))
      .transpose()
      .map_err(|e| Error::from_reason(e.to_string()))?,
    additional_data: loader_context
      .additional_data
      .map(|b| String::from_utf8_lossy(b.as_ref()).to_string()),
    cacheable: loader_context.cacheable,
    file_dependencies: HashSet::from_iter(
      loader_context
        .file_dependencies
        .iter()
        .map(|m| PathBuf::from_str(m).expect("Should convert to path")),
    ),
    context_dependencies: HashSet::from_iter(
      loader_context
        .context_dependencies
        .iter()
        .map(|m| PathBuf::from_str(m).expect("Should convert to path")),
    ),
    missing_dependencies: HashSet::from_iter(
      loader_context
        .missing_dependencies
        .iter()
        .map(|m| PathBuf::from_str(m).expect("Should convert to path")),
    ),
    build_dependencies: HashSet::from_iter(
      loader_context
        .build_dependencies
        .iter()
        .map(|m| PathBuf::from_str(m).expect("Should convert to path")),
    ),
    asset_filenames: HashSet::from_iter(loader_context.asset_filenames.into_iter()),
    // Initialize with no diagnostic
    diagnostics: vec![],

    __resource_data: &ResourceData::new(Default::default(), Default::default()),
    __loader_items: LoaderItemList(list),
    __loader_index: 0,
    __plugins: &[],
  };
  if loader_context.is_pitching {
    // Run pitching loader
    loader
      .pitch(&mut cx)
      .await
      .map_err(|e| Error::from_reason(e.to_string()))?;
  } else {
    // Run normal loader
    loader
      .run(&mut cx)
      .await
      .map_err(|e| Error::from_reason(e.to_string()))?;
  }

  JsLoaderContext::try_from(&cx).map_err(|e| Error::from_reason(e.to_string()))
}

#[napi(object)]
pub struct JsLoaderResult {
  /// Content in pitching stage can be empty
  pub content: Option<Buffer>,
  pub file_dependencies: Vec<String>,
  pub context_dependencies: Vec<String>,
  pub missing_dependencies: Vec<String>,
  pub build_dependencies: Vec<String>,
  pub source_map: Option<Buffer>,
  pub additional_data: Option<Buffer>,
  pub cacheable: bool,
  /// Used to instruct how rust loaders should execute
  pub is_pitching: bool,
}

pub type LoaderThreadsafeLoaderResult = Option<JsLoaderResult>;
