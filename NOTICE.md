# 本 fork 的改动说明

本仓库是 [Zed](https://github.com/zed-industries/zed)（GPL-3.0-or-later）的 fork。`ohos` 分支为 HarmonyOS / OpenHarmony 2in1 增加了平台后端与入口，并替换了界面中指向本项目的名称与链接：

- 新增 `crates/gpui_ohos`（平台后端）与 `crates/zed_ohos`（cdylib 入口 `ohos_gpui_app_main`）；
- 新增 ArkTS + C++ NAPI + XComponent 宿主所需并调用的平台接口（宿主在 `vulkan_shell` 仓库）；
- 界面名称、About 窗口、欢迎页、设置说明与菜单里的 "Zed" 改为 "Inkstone"，去掉指向上游社区、招聘与反馈的入口，应用图标替换为本项目自己的图标。

逐条改动见 `ohos` 分支相对上游的提交记录。发行版由 Inkstone 项目以 GPL-3.0-or-later 分发，与 Zed Industries, Inc. 没有隶属关系。
