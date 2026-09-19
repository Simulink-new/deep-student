# Changelog | 更新日志

All notable changes to **this independent maintenance fork** will be documented in this file.

本文件记录**独立维护版**的所有重要变更。

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
本项目遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

---

## 关于版本号 | Versioning

独立维护版在上游版本号末尾追加修订段：`<上游版本>-fork.<修订号>`

- `0.9.40-fork.1` — 基于上游 `0.9.40` 的第 1 个独立维护版本
- `0.9.40-fork.2`、`0.9.40-fork.3` … — 同上游基线上的后续修订
- 合并上游 `0.9.41` 后 → `0.9.41-fork.1`（上游段完整保留）

---

## [0.9.41](https://github.com/Simulink-new/deep-student/compare/v0.9.40...v0.9.41) (2026-09-19)


### Features

* add backup cancellation support and fix attachment base64 detection ([5b3361e](https://github.com/Simulink-new/deep-student/commit/5b3361e821ba28997ebcf3c79ec7ec52cadf8c18))
* add build support for linux ([#41](https://github.com/Simulink-new/deep-student/issues/41)) ([1d253f2](https://github.com/Simulink-new/deep-student/commit/1d253f25e78aaf7f3c906943bd30e332059ab4a1))
* add ChatAnki integration test plugin for automated testing ([fc20b15](https://github.com/Simulink-new/deep-student/commit/fc20b15f47590cfe3a21dc813821f16125596b0d))
* add data visualization APIs for OCR and text chunk management ([d1b7ae4](https://github.com/Simulink-new/deep-student/commit/d1b7ae4b74f5deb9d5cf564e88c72197e1164083))
* add development scripts for Android environment setup ([ab2953f](https://github.com/Simulink-new/deep-student/commit/ab2953f4bd35ea1ba657154063ff72bf5dcd4d27))
* add DOCX VLM direct extraction path with streaming and checkpoint recovery ([2ee580f](https://github.com/Simulink-new/deep-student/commit/2ee580fd8f8465e9a6b867bc505a3e71f38f1fd4))
* add GitHub Actions workflow for rebuilding Android APK ([1285e99](https://github.com/Simulink-new/deep-student/commit/1285e99643d8f26d61ef2e91d91e11a502e8bd75))
* add image payload parsing and handling utilities ([a16033e](https://github.com/Simulink-new/deep-student/commit/a16033ef6a27041d11de2a743a5c74f91a013079))
* add iPad build workflow + installation guide ([1f04ac6](https://github.com/Simulink-new/deep-student/commit/1f04ac6ae4dccb5bfd16403cde54c884f6abe0e1))
* add memory audit log functionality and enhance memory management ([24cb17b](https://github.com/Simulink-new/deep-student/commit/24cb17ba77e7f37b30506cd6bae10457a27e7f16))
* add multi-tab support with LRU eviction, fix cross-tab event pollution, and enhance LaTeX rendering ([8b349fe](https://github.com/Simulink-new/deep-student/commit/8b349fefa8ef9d64186b4e6208f869b045401d89))
* add native DOCX import with embedded image support ([304d940](https://github.com/Simulink-new/deep-student/commit/304d940663577171f8542db8b86e869f2f1274c4))
* add rebuild-release workflow for manual tag rebuilding ([3d28fec](https://github.com/Simulink-new/deep-student/commit/3d28fec4f6c5fefb794fef3ed2bf2e016a436fb4))
* add save botton to siliconflow section ([#87](https://github.com/Simulink-new/deep-student/issues/87)) ([3bab9cf](https://github.com/Simulink-new/deep-student/commit/3bab9cf725066a67352902a074503f8a41a9434b))
* **ankiCards:** enhance event handling and error reporting ([6f2642c](https://github.com/Simulink-new/deep-student/commit/6f2642c428e1dd2512559f1bb14b6faa20a097ba))
* **chat_v2,workspace,qbank,sync:** add cross-session permission checks and harden tool whitelist bypass ([04a9b10](https://github.com/Simulink-new/deep-student/commit/04a9b10ac9b8a446f811dfc06b5915f386a0a956))
* **chat-v2,learning-hub:** enhance resource handling and state management ([168c253](https://github.com/Simulink-new/deep-student/commit/168c253780c9833c2fd0d6d3e19e63dbe76893f1))
* **chat-v2:** add disable_tool_whitelist option to bypass skill whitelist restrictions ([be13c97](https://github.com/Simulink-new/deep-student/commit/be13c9749874ef2b3369fa7da1fba594f693b798))
* **chat-v2:** add session branching and group pinned resources support ([e56189a](https://github.com/Simulink-new/deep-student/commit/e56189ae77cfafbc045a793a20c19653ac7e4444))
* **chat-v2:** enhance skill state management and event handling ([3c8027a](https://github.com/Simulink-new/deep-student/commit/3c8027aaada91c74f8a98de4b3e915a504f1ffb2))
* **chat-v2:** use dedicated chat_title_model for summary generation with fallback chain ([3759861](https://github.com/Simulink-new/deep-student/commit/3759861ebecfa15ee28747f6fc9818d3d687c2c5))
* **chat,vfs:** add answer submission idempotency and enhance context ref handling ([580db0f](https://github.com/Simulink-new/deep-student/commit/580db0f271f6ad3a03cc18b136e592437a3960cf))
* **cloud-sync:** add real-time upload/download progress events and workspace database backup support ([c9d4bf7](https://github.com/Simulink-new/deep-student/commit/c9d4bf7f6d187d479a5fc646326e8a8f88f7d233))
* **cloud:** 手写 SigV4 S3 客户端——移除 aws-sdk-s3/aws-config 整个依赖树 ([e7dcf6f](https://github.com/Simulink-new/deep-student/commit/e7dcf6f42380efa89ad12388d8fb510df8088081))
* **data_governance:** support virtual URI targets for ZIP exports ([b5bd171](https://github.com/Simulink-new/deep-student/commit/b5bd171fb5a8c16f71797c5bf191c5e25e31a320))
* **debug:** implement debug log persistence and filtering options ([fa8f4c9](https://github.com/Simulink-new/deep-student/commit/fa8f4c9fc99f98ff082890a37beb51ffecbcea5f))
* enhance Anki card handling with action locks, pagination, and improved error handling ([bf5f2bd](https://github.com/Simulink-new/deep-student/commit/bf5f2bd189750f8bd971486fce6ea5673323ec21))
* enhance backup functionality with ImportProgress struct and refactor auto backup logic ([a33f2d9](https://github.com/Simulink-new/deep-student/commit/a33f2d9a5db03e2a467a834cf064d17f0efe890c))
* enhance bidirectional sync with download-first strategy and improved conflict handling ([4fb78e3](https://github.com/Simulink-new/deep-student/commit/4fb78e30737575bdbfafab6c24d432b6939754e0))
* enhance file handling with new extraction utilities ([be86d16](https://github.com/Simulink-new/deep-student/commit/be86d166798455d99cd142808f1c676c4f9cd1a5))
* enhance file name handling and import error reporting ([c167b25](https://github.com/Simulink-new/deep-student/commit/c167b253ee06637c9752ab8437bc30b6d6f9a801))
* enhance image preview handling and improve NoteContentView layout ([ffe392b](https://github.com/Simulink-new/deep-student/commit/ffe392bd44da32a28dd9f5725b335dc3bad6492c))
* enhance memory management with auto extraction and category management ([0b5d8fb](https://github.com/Simulink-new/deep-student/commit/0b5d8fb83158b2811d696852cb6fc7bd07446ace))
* enhance memory management with new relation and tagging features ([d7dc855](https://github.com/Simulink-new/deep-student/commit/d7dc8559ee47cdc253a9f71dbe2998808cf774ad))
* enhance memory management with new settings and export functionality ([2b48b71](https://github.com/Simulink-new/deep-student/commit/2b48b71e3c33e14ec85fb6f8396d4bdca04dbf18))
* enhance MemoryView with batch selection and editing capabilities ([788147e](https://github.com/Simulink-new/deep-student/commit/788147e992bdd368b465253308920c7e78eb1402))
* enhance model capability registry and update related scripts ([9caea57](https://github.com/Simulink-new/deep-student/commit/9caea57694f947c92abca1d5bd02cd4eb24c1697))
* enhance SiliconFlowSection with new OCR model and improve backup functionality ([c5ab6d4](https://github.com/Simulink-new/deep-student/commit/c5ab6d4b1cc9efebe89163c937af53555f48dcc8))
* enhance Smart Memory with self-evolving profile and auto-extraction features ([c29005a](https://github.com/Simulink-new/deep-student/commit/c29005af5e17da3c985bc99e9e510acdddb9d8c5))
* enhance sync functionality with merge strategy and timestamp parsing ([274a81e](https://github.com/Simulink-new/deep-student/commit/274a81ec49a88803d22fd6be6be40d184f813d76))
* enhance tool handling, sleep wake logic, and crypto key backup/restore ([6d27862](https://github.com/Simulink-new/deep-student/commit/6d278624be4bd2454753c88b29fb3cf80f7876ee))
* enhance web search tool with dynamic engine injection ([66b5902](https://github.com/Simulink-new/deep-student/commit/66b590205b828a47f0b449f3b2bd0a608bd6e960))
* **essay-grading:** refine grading mode rubrics and implement progressive hedging for OCR fallback ([9f1c09a](https://github.com/Simulink-new/deep-student/commit/9f1c09a3482aa123e95703a00754075f63642d18))
* **exam:** enhance exam XML generation and qbank tools ([fc80777](https://github.com/Simulink-new/deep-student/commit/fc8077744d99fe66d420552401a69753d6d1b4c6))
* fix tool call handling and user message deduplication in chat history ([6b38748](https://github.com/Simulink-new/deep-student/commit/6b3874895b00d1a15dc3d7d87fd0d3fc9f5fe2ff))
* **gemini,chat-v2,notes,providers:** enhance multimodal handling, cache tokens, and batch import cleanup ([b287a23](https://github.com/Simulink-new/deep-student/commit/b287a237563db711c79e6bda9e7b2933717e6a65))
* **gemini,memory,llm:** add frequency/presence penalties, batch memory write, and provider_scope routing ([958979c](https://github.com/Simulink-new/deep-student/commit/958979c40c4fee5e82e4c9b5cf5161fbb4df8ba0))
* **i18n:** add Todo localization support for en-US and zh-CN ([e61ee8e](https://github.com/Simulink-new/deep-student/commit/e61ee8e561e99ca44fe7b57bac283ad7eaa35494))
* implement auto-extract frequency settings for memory management ([69a5990](https://github.com/Simulink-new/deep-student/commit/69a59905f934cad14416c86571ab4fb20f49193f))
* implement automatic migration for GLM-4.1V to GLM-4.6V model ([2d194d9](https://github.com/Simulink-new/deep-student/commit/2d194d9b35598a1146f418901d02594aa4ff5123))
* implement block and message actions for enhanced chat functionality ([e68df84](https://github.com/Simulink-new/deep-student/commit/e68df84be6dfc0bf9fface0ebfda9929fff25d0e))
* implement content search and session tagging system ([cb846b5](https://github.com/Simulink-new/deep-student/commit/cb846b51741e4fad7ce31d4dfcc0224eba94ff50))
* implement CORS-compliant fetch function for mobile platforms in useAppUpdater ([8206224](https://github.com/Simulink-new/deep-student/commit/8206224ebae1a6efc9afa0689d7559be7c2cb46a))
* implement resource export system with format-specific adapters ([ed6f8f8](https://github.com/Simulink-new/deep-student/commit/ed6f8f834025b6e5356708948c05556c43c60f1e))
* **indexing:** 一键索引自动对预处理未完成的教材/PDF文件执行OCR ([2af03f4](https://github.com/Simulink-new/deep-student/commit/2af03f4de3ed8777ccd59c0fff5423dbd483a125))
* introduce release channel management and update README ([4c47987](https://github.com/Simulink-new/deep-student/commit/4c4798752fa69436f9e16939d015ea2495cc4045))
* **llm:** add model capability registry with automatic vision/tools/reasoning inference ([837aa6c](https://github.com/Simulink-new/deep-student/commit/837aa6ce338d2f9bbd20d98555906b93987249c1))
* **logging:** 运行日志体系对标商业软件大修——统一目录+轮转+运行时可配+崩溃现场+诊断包(task-047) ([5084dcb](https://github.com/Simulink-new/deep-student/commit/5084dcb4dd45af876f56da9d0c09e9881c9a9cfe))
* **memory-system:** hide system-reserved folders/notes with `__*__` pattern across Finder and implement memory folder navigation ([7ddf4c3](https://github.com/Simulink-new/deep-student/commit/7ddf4c3c743a581f25ef72ba36afd3973e8b98f7))
* **memory:** implement write idempotency and enhance data integrity ([bb18278](https://github.com/Simulink-new/deep-student/commit/bb1827852b4018fd51de1c3bd78f6368447413d0))
* **mindmap:** add rich text formatting toolbar and emoji picker, improve node styling and export ([8630e3b](https://github.com/Simulink-new/deep-student/commit/8630e3b9cc0cc61de4b4cc109a6285e216bb3d47))
* **model-watch:** 新模型一键采用——发现区「使用」直通供应商启用条目+能力启发式+api_configurations_changed全端即时刷新 ([ec5fd94](https://github.com/Simulink-new/deep-student/commit/ec5fd946d3f7ee7150e2419f60ded80abd78dce2))
* **model-watch:** 模型新版本每日巡检——/models diff + 新模型发现区(task-046) ([06e530d](https://github.com/Simulink-new/deep-student/commit/06e530dcb28e88bfe4614069976b61f7d00e8d61))
* **notes,textbooks:** detect and sanitize opaque Android document IDs in filenames across frontend and backend ([d75ac97](https://github.com/Simulink-new/deep-student/commit/d75ac976eea724243ed1473bb54b3910fd681669))
* **notes,textbooks:** extract H1 heading from markdown when title is generic placeholder and generate friendly names for opaque document IDs ([c62a022](https://github.com/Simulink-new/deep-student/commit/c62a022aad40ac6da8f2567402038762bcee778a))
* **notes:** add reading mode toggle to prevent keyboard popup on mobile during scrolling ([648d763](https://github.com/Simulink-new/deep-student/commit/648d7636eea2d9d11d0f22effc8922351f91582e))
* **ocr:** add FreeOCR fallback chain with circuit breaker and streamline grading mode prompts ([3fbd852](https://github.com/Simulink-new/deep-student/commit/3fbd852315698344e2675298e0f6dbf777daa14f))
* PDF scan detection + async OCR + image/text toggle ([2928bc7](https://github.com/Simulink-new/deep-student/commit/2928bc7213219e7f24ca992bb60a48336ecb5844))
* **pdf,polyfills:** add Promise.withResolvers polyfill for older browsers and remove unused active feature chips ([aebf481](https://github.com/Simulink-new/deep-student/commit/aebf481d35dd6907e0f380df04c04f7ad6fc50ce))
* **pomodoro:** add immersive focus mode with white noise and circular progress ([2ee581c](https://github.com/Simulink-new/deep-student/commit/2ee581cc41cc55f2053811e414a27310c872d7e0))
* **pomodoro:** add Pomodoro timer support for todo items ([6ad54d9](https://github.com/Simulink-new/deep-student/commit/6ad54d9765f0e7bd7c903525672cd1ba724c3ae8))
* **question-bank:** add question history view and refactor timer management for advanced practice modes ([36746a8](https://github.com/Simulink-new/deep-student/commit/36746a8f124c82d93f61ec95c0723df7d27fdd41))
* **session-management:** introduce session management tools and enhance request handling ([8d26ddb](https://github.com/Simulink-new/deep-student/commit/8d26ddb4eea67203a6fe18d595bc12b8d6014215))
* **settings:** add vendor model batch import and refactor essay grading settings panel ([e6379fd](https://github.com/Simulink-new/deep-student/commit/e6379fd708a757fe2d0e2510ec7ffc29a76db97b))
* **skills-executor:** add custom deserializer to handle stringified array parameters from LLMs ([493677f](https://github.com/Simulink-new/deep-student/commit/493677fe7145a27014ba358ec5ffc3f74969151a))
* standardize Tauri v2 parameter naming to camelCase for automatic snake_case mapping ([64f541c](https://github.com/Simulink-new/deep-student/commit/64f541cbd6d81ce4e03134727678cdcb4362380f))
* sync latest nightly into main for 0.9.40 ([#84](https://github.com/Simulink-new/deep-student/issues/84)) ([53add86](https://github.com/Simulink-new/deep-student/commit/53add861020ad6f1c8ae8d6941036fd8f835f0e5))
* **sync:** add workspace database and VFS blob file-level cloud sync support ([9dc08b5](https://github.com/Simulink-new/deep-student/commit/9dc08b58e2570aba3076bb3d50e64cd4dfdb9ea0))
* **todo:** add comprehensive Todo support across DSTU system ([3863cf9](https://github.com/Simulink-new/deep-student/commit/3863cf9384fc327dc6b27a089e8438ca4f1a61db))
* **todo:** add database constraints and improve code formatting ([2500b9c](https://github.com/Simulink-new/deep-student/commit/2500b9ce34550b131eeb3775da7658c74bd211d9))
* **todo:** add Todo resource type support across Learning Hub ([b8e418d](https://github.com/Simulink-new/deep-student/commit/b8e418dd7225cf48c549c0ed419918da065bb21d))
* **todo:** add user-facing todo system with database schema and system prompt integration ([ba1dfa4](https://github.com/Simulink-new/deep-student/commit/ba1dfa471a1c6ea49c640047e17a41525768a9e9))
* **tools:** add arg_utils for JSON parsing and MCP server configuration ([44c70b4](https://github.com/Simulink-new/deep-student/commit/44c70b4570bffbd087c573ee9be2c37dd1940542))
* update OCR model configurations and enhance engine selection logic ([30097ec](https://github.com/Simulink-new/deep-student/commit/30097ecdb58b9cb24cb3bc03bf32c6b9f55dea7d))
* **vfs:** decouple todo_lists from VFS resources system ([2be0e94](https://github.com/Simulink-new/deep-student/commit/2be0e943b263a0b544009c26f9b4a0121ff1cb4a))
* **vfs:** filter deleted/inactive resources in index status queries and add question filtering in exam uploader ([434fad1](https://github.com/Simulink-new/deep-student/commit/434fad13f1cd6c66925ea9ef3fa2d308d4629595))
* **vfs:** mark resource as pending after successful unit sync ([77c24f1](https://github.com/Simulink-new/deep-student/commit/77c24f1218f402e0290b3de5bc8f199d0ebb3454))
* **workflows:** add hotfix workflow for Linux release assets and improve sync reliability ([4b7a71f](https://github.com/Simulink-new/deep-student/commit/4b7a71fbdc42fec4adc872c86b874713161e6739))
* 全平台包名统一为 com.lanxia.deepstudent — 与 iPad 一致、与上游官方版隔离 ([6ce8b60](https://github.com/Simulink-new/deep-student/commit/6ce8b60ab9336c65129e7987bb971076f26cccc4))
* 同步限流/重试状态透传到前端 — 消除退避等待期的假卡死 ([f62eb07](https://github.com/Simulink-new/deep-student/commit/f62eb07f1754dafdde462599cb75d26d97421f36))
* 对话快照导出/导入后端命令 — meta+消息分页分块导出, 导入全量ID重映射 ([c49ca9b](https://github.com/Simulink-new/deep-student/commit/c49ca9bf3adf469cb9b24db06dc5e40527eac11a))
* 应用图标全套替换为 dist/app-icon.png + iOS 签名配置 ([b21718f](https://github.com/Simulink-new/deep-student/commit/b21718f071efdc8a53231d9eb1a88042572cb45a))
* 接线 5 个命令 — export/import/copy-last-response/read-aloud/sync-now ([0de9147](https://github.com/Simulink-new/deep-student/commit/0de91479b8cae75740320b2dcefb447ff40601bc))
* 桌面端单实例 — 二次启动聚焦已有窗口, 根治双开白屏/双写竞争 ([889345d](https://github.com/Simulink-new/deep-student/commit/889345d71d808e25552819f767de4136ad7a24fb))
* 独立维护版基线 — 0.9.40-fork.1 版本方案 + 断开上游更新通道 + 发布页去上游化 ([4177ab2](https://github.com/Simulink-new/deep-student/commit/4177ab2fa9ac3710a7cf3885bd84ab5247d407d1))
* 题目集导入断点续导（checkpoint resume） ([4b4da2d](https://github.com/Simulink-new/deep-student/commit/4b4da2df3f1e723707bccd793c993f20754ac0f7))


### Bug Fixes

* 3 critical diagnostic findings + translate architecture docs to Chinese ([7e7448b](https://github.com/Simulink-new/deep-student/commit/7e7448bfed32fb1da96d84228fe33be1e1ee0725))
* **a11y:** P3-11 会话列表项补 role=button/tabIndex 与键盘激活 ([6806f42](https://github.com/Simulink-new/deep-student/commit/6806f422d1a60e9188591b3eef36db86b7e823b5))
* add @lobehub/ui and antd dependencies ([c2f43f8](https://github.com/Simulink-new/deep-student/commit/c2f43f8bfd16624491b2ba4d9bc892ffc9515142))
* add & to all adapter instances in streaming_harness tests ([4f4e5a6](https://github.com/Simulink-new/deep-student/commit/4f4e5a644c1919eca46f0f3dd155105f66bbe58b))
* add execute right for build_linux_all.sh ([1d253f2](https://github.com/Simulink-new/deep-student/commit/1d253f25e78aaf7f3c906943bd30e332059ab4a1))
* add fallback logic for empty Anki back field and replace custom scrollbars with CustomScrollArea ([341c9dc](https://github.com/Simulink-new/deep-student/commit/341c9dc6be4553dff604b9192f8a5bbf92714961))
* add onInput handler as secondary paste detection fallback ([d96f7c1](https://github.com/Simulink-new/deep-student/commit/d96f7c15162be7be41a60f39c5a88aa1b5a4ed18))
* add RECORD_AUDIO permission for Android manifest ([#89](https://github.com/Simulink-new/deep-student/issues/89)) ([d2f4424](https://github.com/Simulink-new/deep-student/commit/d2f442488d8a292e0b7d80be4ca2c2b91c723f2b))
* address verified P0/P1 issues from code audit ([0910daa](https://github.com/Simulink-new/deep-student/commit/0910daa38424c37a375a7b9b7df1a84b630a7858))
* align build-test.yml with release.yml build steps ([4f52db8](https://github.com/Simulink-new/deep-student/commit/4f52db86638ac5be68c55bb24b0991fd414b5a5d))
* allow PaddleOCR in get_ocr_model_config pipeline path ([75c9252](https://github.com/Simulink-new/deep-student/commit/75c9252589a02f5e37472b8779840edb6517c901))
* also disable createUpdaterArtifacts in build-test to prevent signing error ([aca0aad](https://github.com/Simulink-new/deep-student/commit/aca0aad9d5f3d3b999f01bf9c2e882524f7042ba))
* Android 构建国内网络优化 — Gradle 腾讯镜像 + Maven 阿里云镜像(幂等注入) ([6abfdd3](https://github.com/Simulink-new/deep-student/commit/6abfdd3c4577595c17c41cf784cb0e25b3a0b4fb))
* **android-files:** support virtual URI import export flows ([58c4234](https://github.com/Simulink-new/deep-student/commit/58c4234762a9fa1eec6c7b3f0672069384c1c646))
* **android:** replace navigator.clipboard with tauri-plugin-clipboard-manager ([80c40fa](https://github.com/Simulink-new/deep-student/commit/80c40fa2f93e2a1e6c893b5dc11d23948976a42e))
* anki card generation error handling + model switch compat ([27f5858](https://github.com/Simulink-new/deep-student/commit/27f58584ff3af14bab9e73b86debdea4d3b75336))
* Anki card tool — upfront model config check with clear error message ([f7623c8](https://github.com/Simulink-new/deep-student/commit/f7623c8b2c30c3c89186c0acb9278e4dd33125f6))
* Anki model config validation + frontend save error handling ([89a8d98](https://github.com/Simulink-new/deep-student/commit/89a8d98ce09e4d0e4814f219d5c253eb79ebfa72))
* Anki tool data chain + session tool compat + API key paste ([b046f3b](https://github.com/Simulink-new/deep-student/commit/b046f3bb0b4c0d01595c62adcf0093bbc11d1a7a))
* **anki,skills:** unify mobile page header navigation config ([#110](https://github.com/Simulink-new/deep-student/issues/110)) ([ce4e6cb](https://github.com/Simulink-new/deep-student/commit/ce4e6cb62ab6223d2907cfa21e84b48da1bcddf1))
* API key paste (InputEvent) + OCR auto-resume after app restart ([84a1c38](https://github.com/Simulink-new/deep-student/commit/84a1c38f96230e4de901483bc463f032d90c6885))
* apply Phase 1 fixes across all 4 areas ([3110c3c](https://github.com/Simulink-new/deep-student/commit/3110c3c865d48821f619856891b38ee0bf4579c0))
* backfill missing VFS tables before change_log pre-repair ([823ab01](https://github.com/Simulink-new/deep-student/commit/823ab01fd64074933d06c79516399add019b8527))
* block OpenAI Responses API on third-party endpoints ([99b92fa](https://github.com/Simulink-new/deep-student/commit/99b92faddd20cd77806db54b61b8b34111a171e6))
* build_android.sh sed 反向引用报错 — NDK 路径统一正斜杠 + cargo 配置探测锚定行首 ([6a87141](https://github.com/Simulink-new/deep-student/commit/6a8714197084f00227a966af115875d9c80b9f8b))
* build_android.sh shasum 缺失回退 sha256sum — Windows Git Bash 无 shasum ([9647bd0](https://github.com/Simulink-new/deep-student/commit/9647bd0a18ff7138810e05784a66c3dd830d5458))
* build_android.sh 优先使用 JAVA_HOME 的 JDK 工具链 ([6ae9dc9](https://github.com/Simulink-new/deep-student/commit/6ae9dc9e06deeb0e75ceeb3a97c90c253f22c9bc))
* build_android.sh 兼容 Windows 版 build-tools (.bat) 的 apksigner 探测 ([88c17e9](https://github.com/Simulink-new/deep-student/commit/88c17e9781dd2fa4bcb19317f1956747427e2cda))
* build-test.yml YAML — all inline with:{} to multi-line syntax ([c3baaba](https://github.com/Simulink-new/deep-student/commit/c3baaba39f97a49902bd28b5bdf56fbe27022cc0))
* **build:** bump Android versionCode to 13516 and add parse_timestamp import ([045703e](https://github.com/Simulink-new/deep-student/commit/045703ef5c454dcce0da62405fab03bc48b5dce2))
* built-in model connectivity test — per-vendor request format adaptation ([e9e40ec](https://github.com/Simulink-new/deep-student/commit/e9e40ec02e6b3126dc257096cbfec3c74d2c16fa))
* cargo check 0 errors - textbook OCR scan support ([7605ac7](https://github.com/Simulink-new/deep-student/commit/7605ac778fbb37dd59883921b468c91a22fee735))
* cargo check passes with 0 errors ([bd11614](https://github.com/Simulink-new/deep-student/commit/bd1161492afd512ba9c7806f1a09e3cb3357c060))
* cargo check 零错误 — 批量修复 24 处类型不匹配与编译错误 ([310909b](https://github.com/Simulink-new/deep-student/commit/310909bf1d1abd2b4a70050ac9df6c32a8e064fe))
* change default web search engine from google_cse to zhipu ([5c713f6](https://github.com/Simulink-new/deep-student/commit/5c713f6805545d803cc7403f0d10c09ad7292823))
* chat display — thinking label jump + message overlap during streaming scroll-up ([ff6a713](https://github.com/Simulink-new/deep-student/commit/ff6a713dfb77592a3e4d39cda59da640cc582ef8))
* **chat-markdown:** restore spacing between streamed blocks ([2ce9a97](https://github.com/Simulink-new/deep-student/commit/2ce9a9772cd7d6208343af57d5067e76434d9455))
* **chat-v2:** enforce explicit model resolution for multimodal injection ([be308bf](https://github.com/Simulink-new/deep-student/commit/be308bf67f0eeb7a3bc14cbf4ef23e7874428434))
* **chat-v2:** ensure active skills content is always passed to backend for synthetic load_skills injection ([5464f52](https://github.com/Simulink-new/deep-student/commit/5464f525a50565783a8bf02c7b833ce1ecbfb49f))
* **chat-v2:** fix continue message error handling and builtin model badge display logic ([c08b30f](https://github.com/Simulink-new/deep-student/commit/c08b30fbe010e9c77b104967d87334ec35876632))
* **chat-v2:** reorder session branching DB writes to satisfy FK constraints and refactor resource picker UI ([2e620ba](https://github.com/Simulink-new/deep-student/commit/2e620ba96614b8118bb0e0db073edc2550585b70))
* **chat:** change SessionCard height from fixed to min-height ([cbb156d](https://github.com/Simulink-new/deep-student/commit/cbb156d89011d51c550762aac35bf142aff725ae))
* **chat:** enforce snapshot import size limit ([8ade06a](https://github.com/Simulink-new/deep-student/commit/8ade06ad13c969e1114fd384bd4b2cf7d844e61c))
* **chat:** tolerate legacy stores without goal fetcher ([260fa59](https://github.com/Simulink-new/deep-student/commit/260fa5967775ffbe47923077fc7bd79134ebcd1f))
* **chat:** 审批已决态即点即出队——APPROVAL_RESOLUTION_DISPLAY_MS 1000ms→0 ([0c8e2c8](https://github.com/Simulink-new/deep-student/commit/0c8e2c8ac723d2a06e09e982f60c808867636589))
* **chat:** 采矿移植 8c7f381d 跟进——fetchGoal 类型断言修复构建门 ([8905325](https://github.com/Simulink-new/deep-student/commit/890532501fe21deb07c093d3ad2bd8b4549f0114))
* CI — E0382 borrow of moved value 'bytes' in generate_preview_on_demand ([c02d61d](https://github.com/Simulink-new/deep-student/commit/c02d61d72934415aa4ffcfc77c869e5aec8b41d0))
* CI — missing return keyword + unused import ([6c07a1d](https://github.com/Simulink-new/deep-student/commit/6c07a1d93b1edb6b77cf3bcf398af7987c41615c))
* CI — model_id scope + comment typo + model clone ([450b17d](https://github.com/Simulink-new/deep-student/commit/450b17dbf4d7686aecf25773e13113abf5491f1e))
* **ci:** add three-path release detection to handle merge commits burying release commit ([466152c](https://github.com/Simulink-new/deep-student/commit/466152c651918718833dbf311a4307ed345fe4c6))
* **ci:** auto-recover android release builds ([ac74c9b](https://github.com/Simulink-new/deep-student/commit/ac74c9be414f2a4b61f22224cfccec7b6d2cf829))
* **ci:** avoid android rebuild invalidation and add heartbeat ([4740877](https://github.com/Simulink-new/deep-student/commit/4740877946eafdabf55c6382c440e4f5be1391e3))
* **ci:** avoid duplicate release creation blocking release-please ([21998cc](https://github.com/Simulink-new/deep-student/commit/21998cc530f566e3d10f40f9e4097578d0c97194))
* **ci:** detect merged release commits with PR suffix ([14547bb](https://github.com/Simulink-new/deep-student/commit/14547bbc726bcabaf4960e2c085582f36b6cb35c))
* **ci:** harden Android build against runner resource exhaustion ([985bc7b](https://github.com/Simulink-new/deep-student/commit/985bc7bc9f7ad4ced66d5d97e56fad3248024ec5))
* **ci:** remove android tee wrapper and add timeout ([79df4e0](https://github.com/Simulink-new/deep-student/commit/79df4e0813b5b3ee405105bda57e45fe96e1b097))
* **ci:** retry transient android dependency failures ([3734c5c](https://github.com/Simulink-new/deep-student/commit/3734c5ce3c5612d6dc65c2cedee84d72da6a88f0))
* **ci:** split sync regression targets across jobs ([#80](https://github.com/Simulink-new/deep-student/issues/80)) ([ed7efb2](https://github.com/Simulink-new/deep-student/commit/ed7efb25c5cf18728693fd88535ea4d5d23064a2))
* **cloud:** S3 手写客户端编译验证修正——Body::empty 私有 API 等 3 处 ([9b6c81d](https://github.com/Simulink-new/deep-student/commit/9b6c81dff5ff048bc3e5e143274371c7a4a16b60))
* correct field references and add missing impl block in debug logger ([13bb819](https://github.com/Simulink-new/deep-student/commit/13bb8194c7d12c9f7a4083c4dacb352a83a54c81))
* correct SQL LIKE pattern escape syntax in note query ([8d96e08](https://github.com/Simulink-new/deep-student/commit/8d96e08bc5bc5cca947e58f7446db68049a7dc2d))
* disable sticky thinking summary during active streaming ([288bdd8](https://github.com/Simulink-new/deep-student/commit/288bdd8f9240ea63536dc8975e44497868ffdf6d))
* E0382 borrow of moved value 'incomplete_ocr_ids' ([235d426](https://github.com/Simulink-new/deep-student/commit/235d426ac36df206799e3092c93073dd35951853))
* enhance error handling and performance optimizations in Chat V2 ([25541cb](https://github.com/Simulink-new/deep-student/commit/25541cb990beedbb7776447a2dd554b1515cc99e))
* **essay-grading:** replace description Input with textarea for multi-line mode descriptions ([1be61db](https://github.com/Simulink-new/deep-student/commit/1be61db9512ae4cc117f928025752c66ac7988be))
* format string arg mismatch + unreachable code ([6a06805](https://github.com/Simulink-new/deep-student/commit/6a06805052071229624182d94268dbb4c67b95ab))
* gate desktop_dir/picture_dir with #[cfg(desktop)] for Android build ([f02f1b3](https://github.com/Simulink-new/deep-student/commit/f02f1b3e058023d5c062cdafbbeed95f78c57dbb))
* **gemini:** add thought_signature support for Gemini 3 tool calling and enforce role alternation ([4559644](https://github.com/Simulink-new/deep-student/commit/45596441ffb24d5ae6b5fd0dc7544f15b1c205b0))
* **gemini:** force v1beta for Gemini 3 models and convert unprotected functionCalls to text ([12d3156](https://github.com/Simulink-new/deep-student/commit/12d3156e0342dea0b62e10aff6e862a78fa8cd54))
* handle release-please comment failure on locked PRs ([6df5ff8](https://github.com/Simulink-new/deep-student/commit/6df5ff895eb80e93157e58f82355821ebf29c494))
* improve question import quality and blob path resolution ([aeb5608](https://github.com/Simulink-new/deep-student/commit/aeb5608115795efbbc99539878d2109ba2f29348))
* increase MCP cache max size for improved performance ([7896e76](https://github.com/Simulink-new/deep-student/commit/7896e76b09d87ed534041e48d43bd31b08be1cd9))
* large PDF loading (271MB+) + OCR progress tracking for all pages ([49f9878](https://github.com/Simulink-new/deep-student/commit/49f98789014a5cc752a13f4397c8d6d822119b41))
* **learning-hub:** 学习资源库全链路核查修复——dstu_set_metadata静默丢失根治+保存结果检查+OCR命令补注册(task-053) ([d4a04eb](https://github.com/Simulink-new/deep-student/commit/d4a04eb6f3f7ed4009bc73135fab9af81e520034))
* LLM error messages now include provider/model/URL context ([2f8f373](https://github.com/Simulink-new/deep-student/commit/2f8f37333f3b749d4f611e10a76b031db8139c8a))
* **llm:** 协议判定 TS 侧补第三方 Responses 守卫——UI 不再谎报可用 ([5189a3e](https://github.com/Simulink-new/deep-student/commit/5189a3e2061efa83fd33968c05318316bed11b75))
* **mcp:** audit compliance fixes - timeout alignment, connection state tracking, and DRY refactor ([814cb91](https://github.com/Simulink-new/deep-student/commit/814cb9189ed108567434ecdf4283fb0133c3d4bb))
* **mcp:** Rust 侧 MCP 协议类型补 camelCase serde rename ([d44b033](https://github.com/Simulink-new/deep-student/commit/d44b03319e2b03baee9a5ae09c5e0475be1c978e))
* **mcp:** sanitize tool names for OpenAI API compatibility and improve memory retrieval ranking ([40bec6f](https://github.com/Simulink-new/deep-student/commit/40bec6f092a009f5e2481eb40f708a6da9cc1d0f))
* **mcp:** 修复 MCP stdio 全链路四个断点——ping schema/重连不刷新/空缓存TTL/启动竞态清洗 ([8e4a5c3](https://github.com/Simulink-new/deep-student/commit/8e4a5c3cdce57c3b278b6c40e991692364293755))
* memory handler bypassing MemoryStorage trait + interface DB ([dd9e8dd](https://github.com/Simulink-new/deep-student/commit/dd9e8dd2555e4d04c23fccf40f93d21addb07f90))
* **memory:** enforce atomic fact storage and prevent knowledge/content leakage ([fd7a0c2](https://github.com/Simulink-new/deep-student/commit/fd7a0c29bd3d71cd0d904f367cf0f50c65fcfcbd))
* merge duplicate clipboardUtils import in useMindMapClipboard ([b747ec3](https://github.com/Simulink-new/deep-student/commit/b747ec328dedf19d932cb1f97d4fa12c6c07b7f0))
* **mindmap:** clamp blank action popup to viewport ([9c63d70](https://github.com/Simulink-new/deep-student/commit/9c63d70dd33d84fb540855d948a50137824a7315))
* **model-watch:** 启动闪退——setup 主线程无 Tokio 上下文,BACKGROUND_TASKS.spawn 即 panic(task-046 回归) ([25bdbeb](https://github.com/Simulink-new/deep-student/commit/25bdbebde13c2b3b5c52b71658ffe943be41572a))
* **model-watch:** 新模型归属修复——全局双索引去重(完整id+短名)+同模型跨供应商合并为多来源条目+聚合平台(NIM)来源排后默认原生供应商 ([50e31e5](https://github.com/Simulink-new/deep-student/commit/50e31e527c033a7c183808048773899fafc724f5))
* **model-watch:** 巡检仅限配置了Key的供应商——无key平台(含内置NVIDIA NIM)不再探测+存量pending自洁(get_state读时过滤+巡检写回) ([be922cd](https://github.com/Simulink-new/deep-student/commit/be922cdd7f88dfb697aea18595907215901df6da))
* module-by-module CI fixes ([3fadec4](https://github.com/Simulink-new/deep-student/commit/3fadec47a2f356a09d2e9747e1e87df437c50b9b))
* N.join is not a function — old ask_user blocks with non-array selected data ([e118d1e](https://github.com/Simulink-new/deep-student/commit/e118d1ed7a6e5d74aa8d9b1b94afce4ef10fbe8c))
* narrow deprecated tool patterns + Zhipu web search improvements ([c85ecd9](https://github.com/Simulink-new/deep-student/commit/c85ecd9683792dc31745f29fdb91479dffd5cab9))
* npm 构建脚本改用 8.3 短路径 Progra~1 — cmd 会剥离含空格引号路径 ([9e28d22](https://github.com/Simulink-new/deep-student/commit/9e28d22d104c6007092b150d0f15979f8ad092b2))
* npm 构建脚本显式调用 Git Bash — 规避 PowerShell 把 bash 解析到 WSL 占位程序 ([db63797](https://github.com/Simulink-new/deep-student/commit/db637975690df5763b86ce7cf79a1c22354ab8a5))
* OCR banner now uses multi-source detection (pdf.js + backend status) ([54b1593](https://github.com/Simulink-new/deep-student/commit/54b1593b54c86b94840f5a057ba468ae487de7e0))
* OCR button now always visible when fileId present and OCR not done ([78650f2](https://github.com/Simulink-new/deep-student/commit/78650f29e95b08a612bdfb314ea4b5b4070a64ed))
* OCR full-chain audit — button visibility, force OCR bypass, diagnostic logging ([d9907bd](https://github.com/Simulink-new/deep-student/commit/d9907bddb9a01da636b677eafe65c92840246b25))
* OCR manual trigger button + scanned PDF banner improvements ([fb28f73](https://github.com/Simulink-new/deep-student/commit/fb28f73da9b4e415d00ed19cd8971ee612d38dd6))
* OCR pipeline for historical scanned PDFs + paste rewrite ([2a22d87](https://github.com/Simulink-new/deep-student/commit/2a22d875a65e95fd9430e7826e56ae32ab33ea6e))
* OCR trigger button now always visible for scanned PDFs ([51aba15](https://github.com/Simulink-new/deep-student/commit/51aba1500095bc44dc67b4369ba98f4d236b9aff))
* **ocr:** 修通PDF解析失败双根因——stage-3存在性门控中和强制重跑+历史50页截断preview自愈(task-049) ([24552ae](https://github.com/Simulink-new/deep-student/commit/24552aeb23af10f7fa1d275f55598129a27bd5c8))
* **ocr:** 重新OCR三症状修复——强制重启流水线+双弹窗消除+进度条事件喂养(task-045) ([8252e5c](https://github.com/Simulink-new/deep-student/commit/8252e5cfc173c0f8da4056632459e5ee84a93521))
* **overview:** 总览图表区无数据时渲染空状态，不再留白 ([09aca28](https://github.com/Simulink-new/deep-student/commit/09aca28005138de9f66b7ef5517e557bbfdef02b))
* PaddleOCR API token fallback from ocr.paddleocr.token setting ([f8192b4](https://github.com/Simulink-new/deep-student/commit/f8192b4cedf2eaba470b0ad46cd4b55157df64ae))
* PaddleOCR connectivity check — GET to POST for POST-only endpoint ([75c529d](https://github.com/Simulink-new/deep-student/commit/75c529d10137ee9c64d0dacf269474d87a023bef))
* PaddleOcrVl/VlV1 no longer incorrectly routed to job-based API ([fa7ebb9](https://github.com/Simulink-new/deep-student/commit/fa7ebb9dde4aa097832a219b849c2b773f3f96ec))
* paste event not detected in empty API key input fields ([eb782de](https://github.com/Simulink-new/deep-student/commit/eb782decafbe74be0068253cfc655dac9e67416b))
* PDF 403 + API key paste detection + GPT-5.5 key gitignore ([bcbe232](https://github.com/Simulink-new/deep-student/commit/bcbe23285785ed4867bc68b923c5a9fddb2f4b7d))
* PDF error type classification + multi-strategy retry + actionable error UI ([eb30843](https://github.com/Simulink-new/deep-student/commit/eb30843a80e9e43d1f9ac9600aecf86b35a68a31))
* PDF pipeline — 5 critical fixes for OCR display, memory leaks, connection pool, and error handling ([a10d9a7](https://github.com/Simulink-new/deep-student/commit/a10d9a73b9b834517ee8543d94b2e5408b4f307e))
* PDF rendering, OCR checkpoint/resume, and progress display ([63a484e](https://github.com/Simulink-new/deep-student/commit/63a484ea8397bb581016b67b9bd08cd2137e6a65))
* **pdf-viewer:** PDF全链路打开修复——streamUrl挂载门闸+CORS暴露头+初始探测200全长(task-052) ([70d1459](https://github.com/Simulink-new/deep-student/commit/70d14590d92f73a092924f5a526051ade27d787b))
* **pdf-viewer:** PDF直连pdfstream流式加载——175MB扫描书秒开+blob扩展名403修复+Document级诊断落盘(task-051) ([4492b52](https://github.com/Simulink-new/deep-student/commit/4492b52d429b79d9cb1fbb3e19ea371df07f28ac))
* **pdf-viewer:** 点开PDF必崩React[#310](https://github.com/Simulink-new/deep-student/issues/310)——hooks越过early return门闸根治+全库同类清零(task-054) ([1f024ab](https://github.com/Simulink-new/deep-student/commit/1f024ab871ea6d8042da16f7c54966993710bb25))
* PDF/OCR pipeline overhaul — scanned PDF rendering + PaddleOCR integration ([2531c9d](https://github.com/Simulink-new/deep-student/commit/2531c9d25146ea5b63c8bcdf434e17653eb56914))
* pin @lobehub/icons to 5.6.0 ([d04fb13](https://github.com/Simulink-new/deep-student/commit/d04fb132ec29b081b93057cf20d11d750b130ebf))
* preserve original PDF as blob regardless of size ([5af3dbe](https://github.com/Simulink-new/deep-student/commit/5af3dbe21a3d4595f6563434062eb5525decd8cf))
* prevent action buttons from overlapping session title during edit ([5278d4b](https://github.com/Simulink-new/deep-student/commit/5278d4beacef6dfa1e63aa85619a490132bf804f))
* prevent duplicate text input during IME composition and sync skill whitelist after load_skills ([05be6b5](https://github.com/Simulink-new/deep-student/commit/05be6b53a1e392174058a3f9afc6e51256bbe942))
* prevent duplicate user messages in history and improve IME handling across platforms ([f903bd1](https://github.com/Simulink-new/deep-student/commit/f903bd18794722fbab566ae932e146cf54428143))
* prevent message stacking during historical session load ([0686144](https://github.com/Simulink-new/deep-student/commit/068614465266604960b7a468e34c5fae928ed482))
* **preview:** 沙箱预览自动高度只涨不缩的棘轮——测量时临时解除 html/body 100% 钉高 ([e9c94bb](https://github.com/Simulink-new/deep-student/commit/e9c94bbe3856e476311863ec5dc13ab0f387bc6c))
* qbank_import_document decode + qbank_ai_grade streaming + image_generate JSON guard ([06df183](https://github.com/Simulink-new/deep-student/commit/06df18309d20fa71751c01cb769a7fedba30e0f4))
* raw string \n escapes + reqwest 0.13 is_dns() removal ([74739cb](https://github.com/Simulink-new/deep-student/commit/74739cbe78d158799afa7c1ace2ce5d83059d904))
* **rebuild:** add --legacy-peer-deps to npm ci ([449a0c2](https://github.com/Simulink-new/deep-student/commit/449a0c2a71cdc0411e18dddaa98c62a757513724))
* reduce streaming render latency — smoother token flow ([9a2961e](https://github.com/Simulink-new/deep-student/commit/9a2961ee50844510d3426a16ad75a2c21f41b524))
* **release:** add --legacy-peer-deps to npm ci ([e0bb680](https://github.com/Simulink-new/deep-student/commit/e0bb680f32b451127962713f83a6641a4bbef371))
* remove 'user message at top' scroll on streaming start ([344af8f](https://github.com/Simulink-new/deep-student/commit/344af8feab5b61c01fd33b71d4e0df8910e2ea77))
* remove deleted sqlite feature from tauri.conf.json build features ([cad1e87](https://github.com/Simulink-new/deep-student/commit/cad1e876ae42769feafe1045a2c7dccd60636584))
* replace whitelist path safety with blacklist for original_path ([9857852](https://github.com/Simulink-new/deep-student/commit/985785222365909f3832ef3b93ed03fcdfaca3ab))
* resolve TypeScript errors in i18n fallbackLng and IndexStatusView ([00a438a](https://github.com/Simulink-new/deep-student/commit/00a438a597816de462e51c6e1ab8e58a65e91951))
* restore stable chunk buffer, targeted measure() for new messages ([c3ee29f](https://github.com/Simulink-new/deep-student/commit/c3ee29f410795888deacdefd6caca507c636c04f))
* **review:** "今日到期"按本地时区取日——东八区晚间不再错一天 ([b4c8890](https://github.com/Simulink-new/deep-student/commit/b4c8890e31882551baae6e6ad4164422fcb5e75e))
* route PaddleOCR base_url requests to job-based API, avoiding 404 on /v1/chat/completions ([975ec34](https://github.com/Simulink-new/deep-student/commit/975ec34d93532ba047fe186f8853a2c80146480a))
* **rust:** canvas 语义搜索 snippet 为 None 时序列化为 null — unwrap_or_default ([a9d82af](https://github.com/Simulink-new/deep-student/commit/a9d82af0dcbe9f63a02fd021e5d5dfa633f3cc65))
* scroll to bottom on session open + fix scroll-back-to-top bug ([5b2afa8](https://github.com/Simulink-new/deep-student/commit/5b2afa8aab0e2b897cd9056180ad5973e720542a))
* search key save button + PDF blob storage for small files ([67e2b3b](https://github.com/Simulink-new/deep-student/commit/67e2b3bc45a0f22c5aa2c89a784e12b73de22a4b))
* session loading compatibility — staged block restore + relaxed validation ([2b2ce50](https://github.com/Simulink-new/deep-student/commit/2b2ce50ac3148543e79841459f083bc3700c96c8))
* session tool compat skip + attachment/note DB + image_generate guard ([c1490d5](https://github.com/Simulink-new/deep-student/commit/c1490d59ec191ab8f6f2c574300d339a9b49e76f))
* **settings:** prevent auto-save from overwriting backend config when loadConfig fails ([21fbb00](https://github.com/Simulink-new/deep-student/commit/21fbb00106e6408e8948f171e78153040fdeab39))
* **skills:** 技能卡片网格补 grid-cols-1，修复移动端卡片横向溢出 21px ([0118513](https://github.com/Simulink-new/deep-student/commit/01185138a2759d395e689df06ba09aca34ebc5f9))
* standardize snippet container heights using Tailwind spacing units ([5fe902d](https://github.com/Simulink-new/deep-student/commit/5fe902d0e60991ebe4aa1a80b597963220995833))
* streaming scroll behavior & text overlap during chat ([8f789d4](https://github.com/Simulink-new/deep-student/commit/8f789d4b76bece6580f7e2e9ed74569ff3931df0))
* String-&gt;AnkiConnectError closure type in test ([2ad0679](https://github.com/Simulink-new/deep-student/commit/2ad0679472cae9eb89042ba18de57fc1eb65b2d8))
* TDZ violation — move PDF cleanup effect after 'file' declaration ([2177f40](https://github.com/Simulink-new/deep-student/commit/2177f403c54a0cea38c7130c4357770753bf8bbd))
* test code — err.contains() → err.to_string().contains() ([b378b81](https://github.com/Simulink-new/deep-student/commit/b378b812b03a2cd803af289c311f7495c9c729a8))
* textbook PDF blob storage & access — fix 403/not-found root causes ([221ec66](https://github.com/Simulink-new/deep-student/commit/221ec66364190a57af604664ed65002f2bcf17a5))
* ToolError→String 类型转换 — ToolResultInfo::failure 错误参数统一 .to_string() ([6d22293](https://github.com/Simulink-new/deep-student/commit/6d2229341d0f29ea8f4df913d314ad2d319212b5))
* TS1128 syntax error in EngineSettingsSection.tsx ([38d26e7](https://github.com/Simulink-new/deep-student/commit/38d26e7b56250b5155c785f60423c3c42eaf1f35))
* TypeScript TS2322/TS2739 — throttledStorage PersistStorage type incompatibility ([7816e68](https://github.com/Simulink-new/deep-student/commit/7816e68e3c32e17e3a791d66f3aa7f3ab9fe2df7))
* unreachable code in SSE transport infinite loop ([84c2059](https://github.com/Simulink-new/deep-student/commit/84c2059691f9f83b5e394a543afd2ecd6afff30e))
* update links in README_EN.md for Quick Start and User Guide ([f4611a5](https://github.com/Simulink-new/deep-student/commit/f4611a5e61463fc88642d30763774b4213e16659))
* update model capabilities and context token limits ([545d645](https://github.com/Simulink-new/deep-student/commit/545d64551045f305139be231fa6621cbc4897a5e))
* update SiliconFlow website URLs in ApisTab and builtin_vendors ([aa2ad0d](https://github.com/Simulink-new/deep-student/commit/aa2ad0dcb6325b647d0ffbecd08b2047d5ec41c7))
* use adapter-transformed request body for LLM request logging ([a93ed02](https://github.com/Simulink-new/deep-student/commit/a93ed02f9e45c52352035628273196623894cac9))
* user_todo_create_item dual alias + PDF split design ([449edce](https://github.com/Simulink-new/deep-student/commit/449edcec592fc742ad8ed226c8b84cac2a8dea39))
* **web-search:** remove engine/force_engine from schema and add silent fallback for unconfigured engines ([c33ca0d](https://github.com/Simulink-new/deep-student/commit/c33ca0da55544acd0af369621ec2e0500bcfd0b4))
* WebDAV 同步 error sending request — 流式上传补 Content-Length + 全链路重试 + 完整错误链 ([7fe06d4](https://github.com/Simulink-new/deep-student/commit/7fe06d4a69ed9b5982ea2e203788e8ba2d3478e3))
* WebDAV 连接检测误报 403 — 探测方法从 GET 目录改为 PROPFIND Depth:0 ([0ca3592](https://github.com/Simulink-new/deep-student/commit/0ca35928186b170f07ab8a8cf2eb27230e51ffb1))
* **webdav:** 同一 provider 的限流滑窗跨 storage 实例共享 ([b1af587](https://github.com/Simulink-new/deep-student/commit/b1af587abad2212892a325acfd5751a4b36195d7))
* Windows 打包固定为 NSIS — MSI 不兼容 -fork.N 版本号 ([28ef16f](https://github.com/Simulink-new/deep-student/commit/28ef16f73ba0c1460e18708b267706d13fb877f9))
* 两个迁移文件内容是指路文本而非 SQL — 全新安装迁移失败根因 ([136c5c6](https://github.com/Simulink-new/deep-student/commit/136c5c67cbd454a3c9997070d830374d91e486e6))
* 云同步卡死修复 — 文件进度真实比例推进 + WebDAV 坚果云滑窗限速器 ([dd46457](https://github.com/Simulink-new/deep-student/commit/dd46457e5289e8036e15dfc62cfd8c7c78b22a02))
* 会话右键菜单补导出对话 — 实际生效的是 ModernSidebar 而非 SessionItemRenderer ([faf0f68](https://github.com/Simulink-new/deep-student/commit/faf0f68524ef5d9a4d37bf25853402c801a9ff80))
* 修复外部模板导入永远失败 — invoke 参数缺 request 包装层 (决策点1) ([9d1bd59](https://github.com/Simulink-new/deep-student/commit/9d1bd59abe32f6e57fadbf13a030d3ad5f574d20))
* 修复错误类型标准化后的类型不匹配 CI 编译错误 ([9fc91c6](https://github.com/Simulink-new/deep-student/commit/9fc91c687e7381f7274b324a41441d98e0333ff9))
* 修正在学习资源内题库中答题结束的祝贺弹窗在移动端的错误位置 ([#51](https://github.com/Simulink-new/deep-student/issues/51)) ([f6690e9](https://github.com/Simulink-new/deep-student/commit/f6690e960585f0338d96b95146479ec3566c036b))
* 制卡 NewCard 事件合并缓冲 — O(N²) 渲染降为 O(N) ([2b2d96e](https://github.com/Simulink-new/deep-student/commit/2b2d96ef7cf0cd0ed470788767ff3d674d98fa26))
* 同步进度条卡在中间不动 — 文件级同步接入字节级进度 + 进度带单调化 ([b646f56](https://github.com/Simulink-new/deep-student/commit/b646f566aa30bf6e395e4ecac8b65fe2fb78b531))
* 同步部分文件失败(503) — 目录创建缓存 + 限流退避重试 ([3a1b41b](https://github.com/Simulink-new/deep-student/commit/3a1b41b134a65eeaed8b27620363fdb820fde3cd))
* 备份/恢复加密密钥路径错位 — 槽位架构下 .secure/.master_key 读写位置修正 ([00255be](https://github.com/Simulink-new/deep-student/commit/00255beb21d58dda0ad4f987e979e01f62e212cd))
* 安卓云服务密码框粘贴失败并被踢回主界面 ([cb6b581](https://github.com/Simulink-new/deep-student/commit/cb6b5812d9ae9f867d9e89c884101c76e96164f1))
* 安卓导入本地备份卡死 — 进度事件洪水 + 阻塞 async runtime ([3c134a7](https://github.com/Simulink-new/deep-student/commit/3c134a71c4bf8b11a2fddadea3b72733878c0a5a))
* 批量修复类型不匹配、非穷尽模式及未使用变量 CI 错误 ([7118fbd](https://github.com/Simulink-new/deep-student/commit/7118fbdcbe9acb4eeb9276661c477e10ba2325fe))
* 清除 VfsError 构造后全部 .to_string() 及中文引号转义遗漏 ([0679486](https://github.com/Simulink-new/deep-student/commit/06794866a430eb731960b9f22c0f67a4ae311d93))
* 移动端云凭据存储统一至活动槽位 + 凭据命令全链路追踪日志 ([cbdcb86](https://github.com/Simulink-new/deep-student/commit/cbdcb868894de92d9a94badb5b47fed5ca231478))
* 移动端打包配置审计修复 — 跨平台 NDK 探测 + 清除陈旧链接器 + 能力补齐 ([c9a866f](https://github.com/Simulink-new/deep-student/commit/c9a866f6f051b484d6c42c98ad55fbd74adb804c))


### Performance Improvements

* **api-config:** ApiConfig id 投影命令+发送路径 30s 缓存——明文 key 不再随每条消息过 IPC ([9b34add](https://github.com/Simulink-new/deep-student/commit/9b34adddd66c68789d6fb692f28384435d59f8cb))
* **attachments:** 拖拽附件按路径直传 VFS——文件字节跨界归零 ([6cf2ba3](https://github.com/Simulink-new/deep-student/commit/6cf2ba3c633a67a6465a47a560d9f9cce88a0e6b))
* **backup/media/notes:** 速赢包A——备份步进More不睡眠(200MB库省25.6s)+PDF双名连发删除(4处legacy emit+2前端监听文件)+notes_save_asset双键base64减半 ([215a048](https://github.com/Simulink-new/deep-student/commit/215a0482b5b2ebefdfa81cdabeb984a39bcbcd66))
* **backup:** manifest 拆分 summary+assets.ndjson——列表页免全量明细解析 ([3bd679b](https://github.com/Simulink-new/deep-student/commit/3bd679b0486611507a50cec53c55fa738fc2b080))
* **backup:** 资产单遍复制哈希+ZIP边压边算+按ID直读manifest——备份/恢复 I/O 削减 ([8d7558f](https://github.com/Simulink-new/deep-student/commit/8d7558f158f034241cb31060eaf5b5b25b47f84f))
* **bundle:** optimize initial load performance with lazy loading and selective subscriptions ([0da3cba](https://github.com/Simulink-new/deep-student/commit/0da3cbab0f0d3a1b7ebd8315d3354c1c31f88d83))
* **chat-fe:** 会话懒加载全链接线+图片预览两级LRU缓存 ([0b88d40](https://github.com/Simulink-new/deep-student/commit/0b88d40a249e6e76dc6c9b25af9d4d9ef1b45fac))
* **chat-v2:** tool_loop 递归改迭代+消息前缀跨轮共享——每轮全量 clone 消灭 ([677f8c2](https://github.com/Simulink-new/deep-student/commit/677f8c2e039951cc976cdc4dc5f7bd21db3270ac))
* **chat-v2:** 工具轮降拷贝——中间保存增量化+多变体伪Arc真共享 ([025a3d1](https://github.com/Simulink-new/deep-student/commit/025a3d156792bc484a86865dffc29879a6a49e92))
* **chat/timeline:** thinking 正文限高 min(60vh,320px) 可滚动 ([6f0fd7e](https://github.com/Simulink-new/deep-student/commit/6f0fd7e3c68ea80b0f8eef2a5710ccb49f75bcf3))
* **chat:** LLM chunk 事件 Rust 侧合批——4096B/16ms 双阈值,序列号无断档 ([7519dd9](https://github.com/Simulink-new/deep-student/commit/7519dd9ce79bb0bf32d0e6eaca8bc68565448add))
* **chat:** 流式防闪退落盘下沉 Rust 侧——删除前端 5s 全量回声 IPC ([cd091b3](https://github.com/Simulink-new/deep-student/commit/cd091b3e31d7832370b7d75325050cd8c56e7ae8))
* **crepe:** ZWS 剥离条件化——IME 外输入免全串复制 ([2577088](https://github.com/Simulink-new/deep-student/commit/25770888cbb3b3f7b77b327b3aab113665038a6c))
* **drag-drop:** 拖拽链路径直传+pathsOnly 提针——三段字节往返与 165MB 瞬时内存消灭(task-040/A10[#1](https://github.com/Simulink-new/deep-student/issues/1)/B6[#1](https://github.com/Simulink-new/deep-student/issues/1)) ([0527dcd](https://github.com/Simulink-new/deep-student/commit/0527dcd686836dca2470b506a499bc3edc4fc436))
* **dstu/finder:** finderStore 批量化+全量刷新节流——最近视图 N+1 消灭+风暴期刷新降频 ([a81921e](https://github.com/Simulink-new/deep-student/commit/a81921e44994ebbcc60a120b927930d1258d6aa2))
* **dstu:** 内容读取 base64 惰性加载+题目集盘读 spawn_blocking ([624b42e](https://github.com/Simulink-new/deep-student/commit/624b42eeeaffe4c09bd2d19dd5c4a261ff90f04d))
* **events:** 速赢包B——孤儿事件族与死代码清理(35个emit点+4死监听+3死文件) ([b0b7d3f](https://github.com/Simulink-new/deep-student/commit/b0b7d3f43e5a601ae1530c70fa25e37c1ac2ff57))
* **fe-stats:** 统计聚合 SQL 下推+题集并行分页+research契约修复+focus节流 ([1755cf0](https://github.com/Simulink-new/deep-student/commit/1755cf01a0deeb93aa8583c3f7fc1965fdf250fb))
* incremental session loading — batch restore + adapter pool + async persist + skeleton UI ([48b0d1a](https://github.com/Simulink-new/deep-student/commit/48b0d1aceb4cdd2588858205b75dfc5dc2ddae6e))
* **lance/multimodal:** 速赢包C——写-only缓存与死代码清理(-400MB启动内存, -3764行) ([8f05ca7](https://github.com/Simulink-new/deep-student/commit/8f05ca710d7266e368f5b1d6bbf8274100b61815))
* **llm-audit:** llm_request_body 回声治理——O(prompt)每轮回声默认关闭+rawRequests有界 ([ed7617d](https://github.com/Simulink-new/deep-student/commit/ed7617df6dbcd37ecd2861bb098dc17ea9b2db57))
* **llm-manager:** api_configs 60s 缓存+请求体单次序列化+死计算删除 ([21d6d33](https://github.com/Simulink-new/deep-student/commit/21d6d33390447e06bb62ef3b042ea8ba5f4854c9))
* **memory:** 写路径治理——分类刷新风暴修复+Fact 双索引消除 ([2b76a42](https://github.com/Simulink-new/deep-student/commit/2b76a42537677b9df3099b434b820e801781a101))
* **mindmap:** 拖拽子树判定预计算——mousemove O(N×depth) 全树遍历降为 O(1) ([ebdd7b9](https://github.com/Simulink-new/deep-student/commit/ebdd7b93ff52c70a7d7facb06f14e8c1e0d00e4f))
* optimize view switching with memoization and ref-based state tracking ([2dc59c2](https://github.com/Simulink-new/deep-student/commit/2dc59c2b6a0cb15d2a274579ac91d3108fb787f6))
* **pdf:** 进度持久化节流——每 tick 全量 UPDATE 改变化驱动+2s 兜底 ([0496c24](https://github.com/Simulink-new/deep-student/commit/0496c24616fe147f05b792ba9cab4e129248e84d))
* **stream:** 三管线流式 emit 改增量 delta 协议——O(n²) 传输降为 O(n) ([8792bbb](https://github.com/Simulink-new/deep-student/commit/8792bbb8d229bfe652351e1a659658204d6698d5))
* **sync/images:** 同步哈希 mtime/size 快路径+远程图片缓存 Arc 化带上限 ([f21df7b](https://github.com/Simulink-new/deep-student/commit/f21df7b6165b12a9caa4d02d451fcfce53350947))
* **translation:** popover 流式 emit 改增量 delta——O(n²) 传输降为 O(n) ([4ba851b](https://github.com/Simulink-new/deep-student/commit/4ba851bb7f55cad955baa1ecf58e8c7d9a1e9ba9))
* **vfs-repos:** 列表 SQL 大列原位裁剪——files/attachment/translation 三 repo 十位点 ([f7eb06c](https://github.com/Simulink-new/deep-student/commit/f7eb06cfddab31475d3694dbe7b7f27105bec363))
* **vfs:** optimize index status query with CTE aggregation and add performance indexes ([f2b2c99](https://github.com/Simulink-new/deep-student/commit/f2b2c995468de2ef3eb629cab3083ecf430f0161))
* **views:** 内容命令族迁 pdfstream:// 流式加载——六视图主内容通道免 base64 过 IPC(A4[#1](https://github.com/Simulink-new/deep-student/issues/1)) ([1ccf54b](https://github.com/Simulink-new/deep-student/commit/1ccf54bb12467ab72c4cfb85d0be262bf32d918c))

## [0.9.40-fork.1] — 独立维护版基线（2026-08-29）

> 自本版本起，本仓库由 [Simulink](https://github.com/newmanyouning) 独立维护，
> 基于 [helixnow/deep-student](https://github.com/helixnow/deep-student) `v0.9.40`（fork 基线 commit `a942024d`）。
> 完整 git 历史与原贡献者信息全部保留；本项目继续以 AGPL-3.0-or-later 发布。

### Added 新增
- PaddleOCR 全栈集成（含 `paddleocr_split` 分片识别）
- research 研究模块（Rust `research/` + 前端类型定义 + 迁移 SQL）
- anki_service 独立服务模块
- 写入门控体系（`vfs/write_gate.rs`、`chat_v2/write_gate.rs`、`dstu/write_gate.rs`、`qbank_write_gate.rs`）
- 云同步信封与限速（`data_governance/sync/envelope.rs`、`permit.rs`）— 修复云同步卡死，WebDAV 坚果云滑动窗口限速
- e2e 测试基建（Playwright + Tauri fixture + 冒烟/旅程用例）
- OCR 页面同步与进度落盘（`ocrPageSync.ts`、`progressFlush.ts`）
- iPad 免费签名每周续签脚本（`renew-ipad.sh`）与文档

### Changed 调整
- 应用图标全套替换为 `dist/app-icon.png` + iOS 签名配置
- 移动端云凭据存储统一至活动槽位
- 会话加载兼容性：分阶段块恢复 + 宽松校验
- 清理废弃组件与 features 目录（notes/practice/skills-management/template-management/voice-input 等归档至 `_archive/`）
- 错误类型全面标准化（~637 命令/函数，详见仓库重构记录）

### Fixed 修复
- 对话显示与流式渲染时序问题
- OCR 按钮可见性与全链路触发逻辑
- PDF 阅读相关修复（original_path 安全策略等）

### Security 安全
- 桌面端自动更新改用本仓库独立签名密钥，与上游更新通道隔离

---

## 上游历史（0.8.9 – 0.9.40）

上游版本的全部历史变更记录已从本文件移除，完整内容见
[原项目 CHANGELOG](https://github.com/helixnow/deep-student/blob/main/CHANGELOG.md)，
或直接查阅本仓库保留的完整 git 历史（`git log`，含全部原贡献者署名）。

原项目及全部历史贡献版权归 DeepStudent 原贡献者所有，
本分支在其基础上以 AGPL-3.0-or-later 继续开发，归属声明见 [NOTICE.md](NOTICE.md)。
