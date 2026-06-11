//! Hermit base URL for workflow_matrix (reuses language-matrix OpenAPI paths).

use tokio::sync::OnceCell;

#[path = "hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

static WORKFLOW_MATRIX_HERMIT: OnceCell<String> = OnceCell::const_new();

pub async fn workflow_matrix_hermit_base_url() -> &'static String {
    WORKFLOW_MATRIX_HERMIT
        .get_or_init(|| async {
            hermit_lang_matrix::language_matrix_hermit_base_url()
                .await
                .clone()
        })
        .await
}
