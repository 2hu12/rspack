use std::{
  borrow::Cow,
  fmt::Debug,
  hash::{BuildHasherDefault, Hash},
  io::Write,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  },
};

use bitflags::bitflags;
use dashmap::DashMap;
use derivative::Derivative;
use rspack_error::{
  internal_error, Diagnostic, IntoTWithDiagnosticArray, Result, Severity, TWithDiagnosticArray,
};
use rspack_hash::RspackHash;
use rspack_identifier::Identifiable;
use rspack_loader_runner::{run_loaders, Content, ResourceData};
use rspack_sources::{
  BoxSource, CachedSource, OriginalSource, RawSource, SourceExt, SourceMap, SourceMapSource,
  WithoutOriginalOptions,
};
use rustc_hash::FxHasher;
use serde_json::json;

use crate::{
  contextify, get_context, BoxLoader, BoxModule, BuildContext, BuildInfo, BuildMeta, BuildResult,
  CodeGenerationResult, Compilation, CompilerOptions, Context, DependencyTemplate, GenerateContext,
  GeneratorOptions, LibIdentOptions, LoaderRunnerPluginProcessResource, Module, ModuleDependency,
  ModuleGraph, ModuleIdentifier, ModuleType, ParseContext, ParseResult, ParserAndGenerator,
  ParserOptions, Resolve, SourceType,
};

bitflags! {
  #[derive(Default)]
  pub struct ModuleSyntax: u8 {
    const COMMONJS = 1 << 0;
    const ESM = 1 << 1;
  }
}

#[derive(Debug, Clone)]
pub enum ModuleIssuer {
  Unset,
  None,
  Some(ModuleIdentifier),
}

impl ModuleIssuer {
  pub fn from_identifier(identifier: Option<ModuleIdentifier>) -> Self {
    match identifier {
      Some(id) => Self::Some(id),
      None => Self::None,
    }
  }

  pub fn identifier(&self) -> Option<&ModuleIdentifier> {
    match self {
      ModuleIssuer::Some(id) => Some(id),
      _ => None,
    }
  }

  pub fn get_module<'a>(&self, module_graph: &'a ModuleGraph) -> Option<&'a BoxModule> {
    if let Some(id) = self.identifier() && let Some(module) = module_graph.module_by_identifier(id) {
      Some(module)
    } else {
      None
    }
  }
}

#[derive(Derivative)]
#[derivative(Debug)]
pub struct NormalModule {
  id: ModuleIdentifier,
  /// Context of this module
  context: Context,
  /// Request with loaders from config
  request: String,
  /// Request intended by user (without loaders from config)
  user_request: String,
  /// Request without resolving
  raw_request: String,
  /// The resolved module type of a module
  module_type: ModuleType,
  /// Affiliated parser and generator to the module type
  parser_and_generator: Box<dyn ParserAndGenerator>,
  /// Resource matched with inline match resource, (`!=!` syntax)
  match_resource: Option<ResourceData>,
  /// Resource data (path, query, fragment etc.)
  resource_data: ResourceData,
  /// Loaders for the module
  #[derivative(Debug = "ignore")]
  loaders: Vec<BoxLoader>,
  /// Whether loaders list contains inline loader
  contains_inline_loader: bool,

  /// Original content of this module, will be available after module build
  original_source: Option<BoxSource>,
  /// Built source of this module (passed with loaders)
  source: NormalModuleSource,

  /// Resolve options derived from [Rule.resolve]
  resolve_options: Option<Resolve>,
  /// Parser options derived from [Rule.parser]
  parser_options: Option<ParserOptions>,
  /// Generator options derived from [Rule.generator]
  generator_options: Option<GeneratorOptions>,

  options: Arc<CompilerOptions>,
  #[allow(unused)]
  debug_id: usize,
  cached_source_sizes: DashMap<SourceType, f64, BuildHasherDefault<FxHasher>>,

  code_generation_dependencies: Option<Vec<Box<dyn ModuleDependency>>>,
  presentational_dependencies: Option<Vec<Box<dyn DependencyTemplate>>>,
}

#[derive(Debug, Clone)]
pub enum NormalModuleSource {
  Unbuild,
  BuiltSucceed(BoxSource),
  BuiltFailed(String),
}

impl NormalModuleSource {
  pub fn new_built(source: BoxSource, diagnostics: &[Diagnostic]) -> Self {
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
      NormalModuleSource::BuiltFailed(
        diagnostics
          .iter()
          .filter(|d| d.severity == Severity::Error)
          .map(|d| d.message.clone())
          .collect::<Vec<String>>()
          .join("\n"),
      )
    } else {
      NormalModuleSource::BuiltSucceed(source)
    }
  }
}

pub static DEBUG_ID: AtomicUsize = AtomicUsize::new(1);

impl NormalModule {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    request: String,
    user_request: String,
    raw_request: String,
    module_type: impl Into<ModuleType>,
    parser_and_generator: Box<dyn ParserAndGenerator>,
    parser_options: Option<ParserOptions>,
    generator_options: Option<GeneratorOptions>,
    match_resource: Option<ResourceData>,
    resource_data: ResourceData,
    resolve_options: Option<Resolve>,
    loaders: Vec<BoxLoader>,
    options: Arc<CompilerOptions>,
    contains_inline_loader: bool,
  ) -> Self {
    let module_type = module_type.into();
    let identifier = if module_type == ModuleType::Js {
      request.to_string()
    } else {
      format!("{module_type}|{request}")
    };
    Self {
      id: ModuleIdentifier::from(identifier),
      context: get_context(&resource_data),
      request,
      user_request,
      raw_request,
      module_type,
      parser_and_generator,
      parser_options,
      generator_options,
      match_resource,
      resource_data,
      resolve_options,
      loaders,
      contains_inline_loader,
      original_source: None,
      source: NormalModuleSource::Unbuild,
      debug_id: DEBUG_ID.fetch_add(1, Ordering::Relaxed),

      options,
      cached_source_sizes: DashMap::default(),
      code_generation_dependencies: None,
      presentational_dependencies: None,
    }
  }

  pub fn match_resource(&self) -> Option<&ResourceData> {
    self.match_resource.as_ref()
  }

  pub fn resource_resolved_data(&self) -> &ResourceData {
    &self.resource_data
  }

  pub fn request(&self) -> &str {
    &self.request
  }

  pub fn user_request(&self) -> &str {
    &self.user_request
  }

  pub fn raw_request(&self) -> &str {
    &self.raw_request
  }

  pub fn source(&self) -> &NormalModuleSource {
    &self.source
  }

  pub fn source_mut(&mut self) -> &mut NormalModuleSource {
    &mut self.source
  }

  pub fn loaders_mut_vec(&mut self) -> &mut Vec<BoxLoader> {
    &mut self.loaders
  }

  pub fn contains_inline_loader(&self) -> bool {
    self.contains_inline_loader
  }
}

impl Identifiable for NormalModule {
  #[inline]
  fn identifier(&self) -> ModuleIdentifier {
    self.id
  }
}

#[async_trait::async_trait]
impl Module for NormalModule {
  fn module_type(&self) -> &ModuleType {
    &self.module_type
  }

  fn source_types(&self) -> &[SourceType] {
    self.parser_and_generator.source_types()
  }

  fn original_source(&self) -> Option<BoxSource> {
    self.original_source.clone()
  }

  fn readable_identifier(&self, context: &Context) -> Cow<str> {
    Cow::Owned(context.shorten(&self.user_request))
  }

  fn size(&self, source_type: &SourceType) -> f64 {
    if let Some(size_ref) = self.cached_source_sizes.get(source_type) {
      *size_ref
    } else {
      let size = f64::max(1.0, self.parser_and_generator.size(self, source_type));
      self.cached_source_sizes.insert(*source_type, size);
      size
    }
  }

  async fn build(
    &mut self,
    build_context: BuildContext<'_>,
  ) -> Result<TWithDiagnosticArray<BuildResult>> {
    let mut build_info = BuildInfo::default();
    let mut build_meta = BuildMeta::default();
    let mut diagnostics = Vec::new();

    build_context.plugin_driver.before_loaders(self).await?;

    let loader_result = run_loaders(
      &self.loaders,
      &self.resource_data,
      &[Box::new(LoaderRunnerPluginProcessResource {
        plugin_driver: build_context.plugin_driver.clone(),
      })],
      build_context.compiler_context,
    )
    .await;
    let (loader_result, ds) = match loader_result {
      Ok(r) => r.split_into_parts(),
      Err(e) => {
        self.source = NormalModuleSource::BuiltFailed(e.to_string());
        let mut hasher = RspackHash::from(&build_context.compiler_options.output);
        self.update_hash(&mut hasher);
        build_meta.hash(&mut hasher);
        build_info.hash = Some(hasher.digest(&build_context.compiler_options.output.hash_digest));
        return Ok(
          BuildResult {
            build_info,
            build_meta: Default::default(),
            dependencies: Vec::new(),
            analyze_result: Default::default(),
          }
          .with_diagnostic(e.into()),
        );
      }
    };
    diagnostics.extend(ds);

    let content = if self.module_type().is_binary() {
      Content::Buffer(loader_result.content.into_bytes())
    } else {
      Content::String(loader_result.content.into_string_lossy())
    };
    let original_source = self.create_source(content, loader_result.source_map)?;
    let mut code_generation_dependencies: Vec<Box<dyn ModuleDependency>> = Vec::new();

    let (
      ParseResult {
        source,
        dependencies,
        presentational_dependencies,
        analyze_result,
      },
      ds,
    ) = self
      .parser_and_generator
      .parse(ParseContext {
        source: original_source.clone(),
        module_identifier: self.identifier(),
        module_parser_options: self.parser_options.as_ref(),
        module_type: &self.module_type,
        module_user_request: &self.user_request,
        resource_data: &self.resource_data,
        compiler_options: build_context.compiler_options,
        additional_data: loader_result.additional_data,
        code_generation_dependencies: &mut code_generation_dependencies,
        build_info: &mut build_info,
        build_meta: &mut build_meta,
      })?
      .split_into_parts();
    diagnostics.extend(ds);
    // Only side effects used in code_generate can stay here
    // Other side effects should be set outside use_cache
    self.original_source = Some(original_source);
    self.source = NormalModuleSource::new_built(source, &diagnostics);
    self.code_generation_dependencies = Some(code_generation_dependencies);
    self.presentational_dependencies = Some(presentational_dependencies);

    let mut hasher = RspackHash::from(&build_context.compiler_options.output);
    self.update_hash(&mut hasher);
    build_meta.hash(&mut hasher);

    build_info.hash = Some(hasher.digest(&build_context.compiler_options.output.hash_digest));
    build_info.cacheable = loader_result.cacheable;
    build_info.file_dependencies = loader_result.file_dependencies;
    build_info.context_dependencies = loader_result.context_dependencies;
    build_info.missing_dependencies = loader_result.missing_dependencies;
    build_info.build_dependencies = loader_result.build_dependencies;
    build_info.asset_filenames = loader_result.asset_filenames;

    Ok(
      BuildResult {
        build_info,
        build_meta,
        dependencies,
        analyze_result,
      }
      .with_diagnostic(diagnostics),
    )
  }

  fn code_generation(&self, compilation: &Compilation) -> Result<CodeGenerationResult> {
    if let NormalModuleSource::BuiltSucceed(source) = &self.source {
      let mut code_generation_result = CodeGenerationResult::default();
      for source_type in self.source_types() {
        let generation_result = self.parser_and_generator.generate(
          source,
          self,
          &mut GenerateContext {
            compilation,
            module_generator_options: self.generator_options.as_ref(),
            runtime_requirements: &mut code_generation_result.runtime_requirements,
            data: &mut code_generation_result.data,
            requested_source_type: *source_type,
          },
        )?;
        code_generation_result.add(*source_type, CachedSource::new(generation_result).boxed());
      }
      code_generation_result.set_hash(
        &compilation.options.output.hash_function,
        &compilation.options.output.hash_digest,
        &compilation.options.output.hash_salt,
      );
      Ok(code_generation_result)
    } else if let NormalModuleSource::BuiltFailed(error_message) = &self.source {
      let mut code_generation_result = CodeGenerationResult::default();

      // If the module build failed and the module is able to emit JavaScript source,
      // we should emit an error message to the runtime, otherwise we do nothing.
      if self.source_types().contains(&SourceType::JavaScript) {
        code_generation_result.add(
          SourceType::JavaScript,
          RawSource::from(format!("throw new Error({});\n", json!(error_message))).boxed(),
        );
      }
      code_generation_result.set_hash(
        &compilation.options.output.hash_function,
        &compilation.options.output.hash_digest,
        &compilation.options.output.hash_salt,
      );
      Ok(code_generation_result)
    } else {
      Err(internal_error!(
        "Failed to generate code because ast or source is not set for module {}",
        self.request
      ))
    }
  }

  fn name_for_condition(&self) -> Option<Cow<str>> {
    // Align with https://github.com/webpack/webpack/blob/8241da7f1e75c5581ba535d127fa66aeb9eb2ac8/lib/NormalModule.js#L375
    let resource = self.resource_data.resource.as_str();
    let idx = resource.find('?');
    if let Some(idx) = idx {
      Some(resource[..idx].into())
    } else {
      Some(resource.into())
    }
  }

  fn lib_ident(&self, options: LibIdentOptions) -> Option<Cow<str>> {
    // Align with https://github.com/webpack/webpack/blob/4b4ca3bb53f36a5b8fc6bc1bd976ed7af161bd80/lib/NormalModule.js#L362
    Some(Cow::Owned(contextify(options.context, self.user_request())))
  }

  fn get_resolve_options(&self) -> Option<&Resolve> {
    self.resolve_options.as_ref()
  }

  fn get_code_generation_dependencies(&self) -> Option<&[Box<dyn ModuleDependency>]> {
    if let Some(deps) = self.code_generation_dependencies.as_deref() && !deps.is_empty() {
      Some(deps)
    } else {
      None
    }
  }

  fn get_presentational_dependencies(&self) -> Option<&[Box<dyn DependencyTemplate>]> {
    if let Some(deps) = self.presentational_dependencies.as_deref() && !deps.is_empty() {
      Some(deps)
    } else {
      None
    }
  }

  fn get_context(&self) -> Option<&Context> {
    Some(&self.context)
  }
}

impl PartialEq for NormalModule {
  fn eq(&self, other: &Self) -> bool {
    self.identifier() == other.identifier()
  }
}

impl Eq for NormalModule {}

impl NormalModule {
  fn create_source(&self, content: Content, source_map: Option<SourceMap>) -> Result<BoxSource> {
    if content.is_buffer() {
      return Ok(RawSource::Buffer(content.into_bytes()).boxed());
    }
    if self.options.devtool.enabled() && let Some(source_map) = source_map {
      let content = content.into_string_lossy();
      return Ok(
        SourceMapSource::new(WithoutOriginalOptions {
          value: content,
          name: self.request(),
          source_map,
        })
        .boxed(),
      );
    }
    if self.options.devtool.source_map() && let Content::String(content) = content {
      return Ok(OriginalSource::new(content, self.request()).boxed());
    }
    Ok(RawSource::from(content.into_string_lossy()).boxed())
  }
}

impl Hash for NormalModule {
  fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
    "__rspack_internal__NormalModule".hash(state);
    if let Some(original_source) = &self.original_source {
      original_source.hash(state);
    }
  }
}
