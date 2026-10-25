# Open local files in Finder

- [x] Add an **Open in Finder** action wherever the app shows local files, such
  as file trees, search results, tabs, and file references.
- [x] Open the file's containing folder in Finder and select the file when
  possible; for folders, open the folder itself.
- [x] Keep the action available only for files on the local machine, and define
  clear behavior for remote workspace files.

Implemented in the desktop UI: file-tree and search-result context menus,
file-tab context menus, the editor toolbar, and transcript file-reference menus.
Remote files retain Open in Zeron and Copy path, with no local file-manager
action. See [file interactions](../docs/file-tree-interactions.md).
