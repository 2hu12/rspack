use rspack_core::{
  module_namespace_promise, ChunkGroupOptions, Dependency, DependencyCategory, DependencyId,
  DependencyTemplate, DependencyType, ErrorSpan, ModuleDependency, TemplateContext,
  TemplateReplaceSource,
};
use swc_core::ecma::atoms::JsWord;

#[derive(Debug, Clone)]
pub struct ImportDependency {
  start: u32,
  end: u32,
  id: DependencyId,
  request: JsWord,
  span: Option<ErrorSpan>,
  /// This is used to implement `webpackChunkName`, `webpackPrefetch` etc.
  /// for example: `import(/* webpackChunkName: "my-chunk-name", webpackPrefetch: true */ './module')`
  pub group_options: ChunkGroupOptions,
}

impl ImportDependency {
  pub fn new(
    start: u32,
    end: u32,
    request: JsWord,
    span: Option<ErrorSpan>,
    group_options: ChunkGroupOptions,
  ) -> Self {
    Self {
      start,
      end,
      request,
      span,
      id: DependencyId::new(),
      group_options,
    }
  }
}

impl Dependency for ImportDependency {
  fn id(&self) -> &DependencyId {
    &self.id
  }

  fn category(&self) -> &DependencyCategory {
    &DependencyCategory::Esm
  }

  fn dependency_type(&self) -> &DependencyType {
    &DependencyType::DynamicImport
  }
}

impl ModuleDependency for ImportDependency {
  fn request(&self) -> &str {
    &self.request
  }

  fn user_request(&self) -> &str {
    &self.request
  }

  fn span(&self) -> Option<&ErrorSpan> {
    self.span.as_ref()
  }

  fn group_options(&self) -> Option<&ChunkGroupOptions> {
    Some(&self.group_options)
  }

  fn set_request(&mut self, request: String) {
    self.request = request.into();
  }
}

impl DependencyTemplate for ImportDependency {
  fn apply(
    &self,
    source: &mut TemplateReplaceSource,
    code_generatable_context: &mut TemplateContext,
  ) {
    source.replace(
      self.start,
      self.end,
      module_namespace_promise(code_generatable_context, &self.id, &self.request, false).as_str(),
      None,
    );
  }
}
