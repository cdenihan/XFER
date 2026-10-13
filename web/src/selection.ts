import type { Item, Selection } from "./types";
export function size(bytes: number) {
  if (bytes < 1024) return bytes + " B";
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let index = -1;
  do {
    bytes /= 1024;
    index++;
  } while (bytes >= 1024 && index < units.length - 1);
  return bytes.toFixed(bytes >= 10 ? 0 : 1) + " " + units[index];
}
export function fromFiles(files: FileList | File[]): Selection | null {
  const items = Array.from(files);
  if (!items.length) return null;
  if (items[0].webkitRelativePath)
    return {
      items: items.map((file) => ({ file, path: file.webkitRelativePath })),
      root: items[0].webkitRelativePath.split("/")[0],
      folder: true,
    };
  if (items.length === 1)
    return { items: [{ file: items[0], path: items[0].name }], root: items[0].name, folder: false };
  return {
    items: items.map((file) => ({ file, path: "Shared items/" + file.name })),
    root: "Shared items",
    folder: true,
  };
}
// Append into one shared array; never copy or spread entire directory subtrees.
async function collect(entry: FileSystemEntry, prefix: string, items: Item[]) {
  if (items.length >= 100_000) throw new Error("The selection contains too many entries.");
  const path = prefix + entry.name;
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) =>
      (entry as FileSystemFileEntry).file(resolve, reject),
    );
    items.push({ file, path });
  } else if (entry.isDirectory) {
    items.push({ path, directory: true });
    const reader = (entry as FileSystemDirectoryEntry).createReader();
    while (true) {
      const entries = await new Promise<FileSystemEntry[]>((resolve, reject) =>
        reader.readEntries(resolve, reject),
      );
      if (!entries.length) break;
      for (const child of entries) await collect(child, path + "/", items);
    }
  }
}
export async function walk(entry: FileSystemEntry, prefix = ""): Promise<Item[]> {
  const items: Item[] = [];
  await collect(entry, prefix, items);
  return items;
}
export async function fromDrop(data: DataTransfer): Promise<Selection | null> {
  const entries = Array.from(data.items)
    .map((item) => item.webkitGetAsEntry?.())
    .filter((entry): entry is FileSystemEntry => entry != null);
  if (!entries.length) return fromFiles(data.files);
  const multiple = entries.length > 1;
  const items: Item[] = [];
  for (const entry of entries) await collect(entry, multiple ? "Shared items/" : "", items);
  return {
    items,
    root: multiple ? "Shared items" : entries[0].name,
    folder: multiple || entries[0].isDirectory,
  };
}
