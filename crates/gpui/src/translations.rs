//! Translations for user facing text.
//!
//! The table is static so a lookup keeps the `&'static str` the callers expect
//! and allocates nothing. Entries are sorted by the original text because the
//! lookup is a binary search; a missing entry returns the original unchanged,
//! which is what makes a partially translated build usable.

/// Pairs of (original, translation). Keep sorted by the first element.
static TRANSLATIONS: &[(&str, &str)] = &[
    ("Add Folder to Project", "向项目添加文件夹"),
    ("Close Editor", "关闭编辑器"),
    ("Close Project", "关闭项目"),
    ("Close Window", "关闭窗口"),
    ("Copy", "复制"),
    ("Cut", "剪切"),
    ("Edit", "编辑"),
    ("File", "文件"),
    ("Find", "查找"),
    ("Go", "转到"),
    ("Help", "帮助"),
    ("New", "新建"),
    ("New File", "新建文件"),
    ("New Window", "新建窗口"),
    ("Open File", "打开文件"),
    ("Open Folder", "打开文件夹"),
    ("Open Recent", "打开最近"),
    ("Open Remote", "打开远程"),
    ("Open Settings", "打开设置"),
    ("Paste", "粘贴"),
    ("Quit", "退出"),
    ("Redo", "重做"),
    ("Run", "运行"),
    ("Save", "保存"),
    ("Save All", "全部保存"),
    ("Save As", "另存为"),
    ("Select All", "全选"),
    ("Selection", "选择"),
    ("Undo", "撤销"),
    ("View", "视图"),
    ("Window", "窗口"),
];

/// Translate a user facing string, returning it unchanged when the table has no
/// entry for it.
/// Translate a display name that may carry a namespace.
///
/// Namespaced actions report "namespace::Name" and the editor shows the two
/// parts apart, so the table is keyed by the plain name; when it has no entry
/// the caller's pre-built literal, which already includes the namespace, is
/// returned unchanged.
pub fn tr_scoped(name: &'static str, scoped: &'static str) -> &'static str {
    match TRANSLATIONS.binary_search_by_key(&name, |entry| entry.0) {
        Ok(index) => TRANSLATIONS[index].1,
        Err(_) => scoped,
    }
}

/// Translate a string that carries no namespace, returning it unchanged when
/// the table has no entry for it.
pub fn tr(text: &'static str) -> &'static str {
    match TRANSLATIONS.binary_search_by_key(&text, |entry| entry.0) {
        Ok(index) => TRANSLATIONS[index].1,
        Err(_) => text,
    }
}
