use std::{fmt::Debug, path::Path, sync::Arc};

use rspack_binding_options::{get_builtin_loader, JsLoaderAdapter, JsLoaderRunner};
use rspack_core::{
  BoxLoader, CompilerOptions, NormalModule, Plugin, ResolveResult, Resolver, BUILTIN_LOADER_PREFIX,
};
use rspack_error::{internal_error, Result};

pub struct JsLoaderResolver {
  pub js_loader_runner: JsLoaderRunner,
}

impl Debug for JsLoaderResolver {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("JsLoaderResolver")
      .field("js_loader_runner", &"..")
      .finish()
  }
}

#[async_trait::async_trait]
impl Plugin for JsLoaderResolver {
  async fn before_loaders(&self, module: &mut NormalModule) -> Result<()> {
    let contains_inline = module.contains_inline_loader();
    let old_loaders = module.loaders_mut_vec();
    if old_loaders.is_empty() || old_loaders.len() == 1 {
      return Ok(());
    }

    // If there's any JS loader, then we switch to the JS loader runner.
    // Else, we run loader on the Rust side using the Rust loader runner.
    // Note: If the loaders list contains inline loaders,
    // fallback to JS loader runner for passing builtin options(reuse Compiler.ruleSet).
    if contains_inline
      || old_loaders
        .iter()
        .any(|l| !l.identifier().starts_with(BUILTIN_LOADER_PREFIX))
    {
      *module.loaders_mut_vec() = vec![Arc::new(JsLoaderAdapter {
        runner: self.js_loader_runner.clone(),
        identifier: old_loaders
          .iter()
          .map(|l| l.identifier().as_str())
          .collect::<Vec<_>>()
          .join("$")
          .into(),
      })];
    }

    Ok(())
  }

  async fn resolve_loader(
    &self,
    _compiler_options: &CompilerOptions,
    context: &Path,
    resolver: &Resolver,
    loader_request: &str,
    loader_options: Option<&str>,
  ) -> Result<Option<BoxLoader>> {
    let mut rest = None;
    let prev = if let Some(index) = loader_request.find('?') {
      rest = Some(&loader_request[index..]);
      Path::new(&loader_request[0..index])
    } else {
      Path::new(loader_request)
    };

    if loader_request.starts_with(BUILTIN_LOADER_PREFIX) {
      return Ok(Some(get_builtin_loader(loader_request, loader_options)));
    }

    let resolve_result = resolver
      .resolve(context, &prev.to_string_lossy())
      .map_err(|err| {
        let loader_request = prev.display();
        let context = context.display();
        internal_error!("Failed to resolve loader: {loader_request} in {context} {err:?}")
      })?;

    match resolve_result {
      ResolveResult::Resource(resource) => {
        // TODO: Should move this logic to `resolver`, since `resolve.alias` may contain query or fragment too. @Boshen
        let resource = resource.path.to_string_lossy().to_string() + rest.unwrap_or_default();
        Ok(Some(Arc::new(JsLoaderAdapter {
          identifier: resource.into(),
          runner: self.js_loader_runner.clone(),
        })))
      }
      ResolveResult::Ignored => {
        let loader_request = prev.display();
        Err(internal_error!(
          "Failed to resolve loader: {loader_request}"
        ))
      }
    }
  }
}
