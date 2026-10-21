<p align="right">🌐 <a href="README.md">🇬🇧 English</a> · <b>🇻🇳 Tiếng Việt</b></p>

# LLM Wiki RS (`llm-wiki-rs`)

Thư viện Rust local-first kết hợp kiến trúc **[LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)** của Andrej Karpathy với động cơ xử lý & tìm kiếm **[QMD (Query Markup Documents)](https://github.com/tobi/qmd)**, tương thích 100% với **[Obsidian's Graph View](https://obsidian.md)**.

- **Crate**: [`llm-wiki-rs`](https://crates.io/crates/llm-wiki-rs) `v0.1.1` (import trong Rust là `llm_wiki`), edition 2021
- **Lưu trữ**: SQLite nhúng (`rusqlite` bundled + FTS5 BM25 + vector cosine distance + bảng đồ thị liên kết) nằm gọn tại `<data_root>/.wiki/index.sqlite`
- **Tài liệu kiến trúc**: [ARCHITECTURE.md](./ARCHITECTURE.md) · [docs/karpathy-llm-wiki.md](./docs/karpathy-llm-wiki.md)

## Tính Năng Nổi Bật

| Nhóm | Mô tả |
|---|---|
| **Khởi tạo 1 thư mục duy nhất** | `WikiEngine::open(data_root)` tự động khởi tạo `sources/`, `wiki/`, `SCHEMA.md`, `wiki/index.md`, `wiki/log.md`, `.obsidian/graph.json` và `.wiki/index.sqlite`. |
| **QMD Smart Chunking** | Cắt chunk (~900 tokens, 15% overlap) theo điểm ngắt ngữ nghĩa (Heading `H1`–`H6`, đoạn văn, đường kẻ ngang), suy giảm khoảng cách bình phương và **Code Fence Protection** (không bao giờ cắt ngang khối code ```` ``` ````). |
| **QMD Hybrid Search** | Kết hợp **SQLite FTS5 (BM25)** + **Vector Search** (`vec_cosine_distance`) + **Reciprocal Rank Fusion (RRF, $k=60$)** ($2\times$ trọng số cho truy vấn đầu, thưởng hạng Top-1 $+0.05$, Top-2/3 $+0.02$) + **Position-Aware Reranker Blending**. |
| **Mặc định tìm trên `wiki` & Trả về liên kết `[[WikiLinks]]`** | `query` / `wiki_query` mặc định tìm trên `collections: ["wiki"]` (truyền `["sources"]` hoặc `["wiki", "sources"]` khi cần tìm trong raw sources). Mỗi `SearchResultItem` đính kèm sẵn `is_wiki_linked`, `outgoing_links`, `backlinks`, và `unresolved_links`. |
| **Metadata AST Filter & Discovery** | Đánh chỉ mục YAML frontmatter và `qmd.metadata`, lọc đệ quy theo biểu thức logic (`and`, `or`, `not`, `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `in`, `nin`, `all`, `contains`, `prefix`, `suffix`, `exists`, `type`). |
| **Obsidian Graph View** | Hỗ trợ đầy đủ `[[WikiLinks]]`, `[[path#Heading\|Alias]]`, `![[Embeds]]`, liên kết trong YAML Frontmatter (`source_file: "[[sources/...\|...]]"`), phân nhánh 1 cấp `<domain>` (`wiki/entities/<domain>/`, `wiki/concepts/<domain>/`, `wiki/syntheses/<domain>/`), chống trùng slug toàn cục và tự động chuẩn hóa đường dẫn WikiLink trên đĩa. |
| **Giữ nguyên tên file Source khi Ingest** | Khi ingest `sources/docs/flow/lib.md`, trang tóm tắt trong `wiki/` được tạo tại `wiki/sources/docs/flow/lib.md` (giữ nguyên tên file `lib.md`, tuyệt đối không sinh ID ngẫu nhiên làm tên file). |
| **Tự động tạo folder lồng & Tree API** | Truyền đường dẫn `"docs/flow/lib.md"` tự động kiểm tra chống Path Traversal (`..`) và tạo các folder cha. `get_tree()` trả về `FileTreeNode` đệ quy + `.to_ascii_tree()`. |
| **13 LLM Tools & Adaptive Read Budget** | Cung cấp JSON Schema (`llm_tool_definitions()`) và `execute_llm_tool_with_budget()` kèm `AdaptiveReadBudget`, danh mục gọn ưu tiên theo chủ đề (`read_compact_index_md_for_topic`) và luồng điều phối tự động (`build_next_autoflow_prompt`). |

## Cài Đặt

Thêm `llm-wiki-rs` vào `Cargo.toml`:

```toml
[dependencies]
llm-wiki-rs = "0.1.1"
```

> Lưu ý: Tên thư viện trong code Rust là `llm_wiki` (`use llm_wiki::*;`).

## Khởi Đầu Nhanh (Quick Start)

```rust
use llm_wiki::{
    AdaptiveReadBudget, HybridQueryRequest, IngestSourceRequest, TreeOptions, TreeScope, WikiEngine,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Mở hoặc khởi tạo Vault tương thích Obsidian chỉ với 1 đường dẫn thư mục
    let engine = WikiEngine::open("./my_knowledge_vault")?;

    // 2. Tạo file nguồn lồng nhau (tự động tạo `sources/docs/flow/`)
    engine.write_source_file(
        "docs/flow/lib.md",
        "# Flow Engine\n\nFlow Engine được viết bằng Rust và dùng SQLite FTS5.",
    )?;

    // 3. Ingest vào `wiki/` (giữ nguyên tên file `lib.md` tại `wiki/sources/docs/flow/lib.md`)
    engine.ingest_source_to_wiki(IngestSourceRequest {
        source_rel_path: "docs/flow/lib.md".to_string(),
        target_wiki_path: None,
        raw_source_content: None,
        title: "Kiến trúc Flow Engine".to_string(),
        one_line_summary: "Tóm tắt kiến trúc của Flow Engine.".to_string(),
        wiki_markdown_body: "Flow Engine sử dụng [[rust]] và [[bm25-search]] để tìm kiếm lai."
            .to_string(),
        tags: vec!["rust".to_string(), "search".to_string()],
        related_entities: vec!["rust".to_string()],
        related_concepts: vec!["bm25-search".to_string()],
    })?;

    // 4. Tìm kiếm (`collections` mặc định là `["wiki"]`, kết quả kèm sẵn quan hệ WikiLink)
    let hits = engine.query(HybridQueryRequest::from_query("Flow Engine tìm kiếm lai"))?;
    for hit in &hits {
        println!(
            "{} ({}) score={:.2} is_wiki_linked={} outgoing={:?} backlinks={:?}",
            hit.rel_path, hit.docid, hit.score, hit.is_wiki_linked, hit.outgoing_links, hit.backlinks
        );
    }

    // 5. Xem cây thư mục
    let tree = engine.get_tree(TreeOptions::for_scope(TreeScope::All))?;
    println!("{}", tree.to_ascii_tree());

    // 6. Thực thi LLM Tool kèm ngân sách đọc thích ứng (AdaptiveReadBudget)
    let budget = AdaptiveReadBudget::for_context_limit(200_000);
    let out = engine.execute_llm_tool_with_budget(
        "wiki_query",
        json!({ "query": "Flow Engine" }),
        budget,
    )?;
    println!("{}", out.content);

    Ok(())
}
```

## Chạy Kiểm Thử & Ví Dụ

```bash
# Chạy toàn bộ test suite
cargo test

# Chạy ví dụ minh họa toàn bộ luồng hoạt động cơ bản
cargo run --example full_workflow

# Chạy ví dụ Live YouTube Playlist -> Raw Source Markdown -> Karpathy LLM Wiki (dùng orchestrator-agent)
cp .env.live.example .env.live   # điền G0_OPENAI_BASE_URL, G0_OPENAI_MODEL, G0_OPENAI_API_KEY
cargo run --example youtube_playlist_wiki -- "<PLAYLIST_URL>" --limit 3 --lang en,vi

# Chạy ví dụ PDF / Tài liệu -> anydoc (GFM Markdown) -> Multi-Subagent -> Karpathy LLM Wiki
cargo run --example pdf_anydoc_wiki -- "<PDF_PATH>" --max-chars 60000
# Hoặc chạy thử ngay với file PDF kỹ thuật mẫu tự sinh:
cargo run --example pdf_anydoc_wiki -- --sample-pdf
```

## Tài Liệu Chi Tiết

- [ARCHITECTURE.md](./ARCHITECTURE.md) — Đặc tả kỹ thuật, SQLite schema, thuật toán QMD & Rust API.
- [docs/karpathy-llm-wiki.md](./docs/karpathy-llm-wiki.md) — Nguyên bản ý tưởng "LLM Wiki" của Andrej Karpathy và bảng ánh xạ vào `llm-wiki-rs`.
- [examples/youtube_playlist_wiki/](./examples/youtube_playlist_wiki/main.rs) — Ứng dụng mẫu tích hợp [`orchestrator-agent`](https://crates.io/crates/orchestrator-agent) để xử lý YouTube Playlist thành Raw Sources (`sources/<domain>/<topic>/`) và tự động tổng hợp, liên kết chéo `[[WikiLinks]]`, kiểm tra sức khỏe (`lint_wiki`) trong `wiki/`.
- [examples/pdf_anydoc_wiki/](./examples/pdf_anydoc_wiki/main.rs) — Ứng dụng mẫu dùng [`anydoc`](https://crates.io/crates/anydoc) để chuyển đổi file PDF (hoặc DOCX/EPUB/PPTX/XLSX) sang GitHub-Flavored Markdown, phân loại vào `sources/<domain>/<topic>/` và điều phối Multi-Subagent biên dịch sang `wiki/`.

## Giấy Phép (License)

Phân phối theo giấy phép **MIT** hoặc **Apache-2.0** tùy bạn chọn.
