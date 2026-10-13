import { describe, it, expect, vi } from "vite-plus/test";
import { sendSelection, type Api } from "./api";
import { fromFiles, walk } from "./selection";
import type { Selection } from "./types";
const selection: Selection = {
  root: "bundle",
  folder: true,
  items: [
    { path: "bundle/empty", directory: true },
    { path: "bundle/hello.txt", file: new File(["hello"], "hello.txt") },
  ],
};
function mockApi(fail?: string) {
  const fn = vi.fn(async (path: string) => {
    if (path === fail) throw new Error("interrupted");
    return {};
  });
  return { fn, api: fn as Api };
}
describe("upload ownership", () => {
  it("never cancels an incoming job when /new rejects the selection", async () => {
    const { fn, api } = mockApi("new");
    await expect(sendSelection(api, selection, "peer", vi.fn())).rejects.toThrow("interrupted");
    expect(fn.mock.calls.map((call) => call[0])).toEqual(["new"]);
  });
  it.each(["directory", "upload", "send"])(
    "cleans up owned staging after %s fails",
    async (fail) => {
      const { fn, api } = mockApi(fail);
      await expect(sendSelection(api, selection, "peer", vi.fn())).rejects.toThrow("interrupted");
      expect(fn.mock.calls.at(-1)?.[0]).toBe("cancel");
    },
  );
  it("preserves directories, encodes paths and hands off exactly once", async () => {
    const { fn, api } = mockApi();
    const progress = vi.fn();
    await sendSelection(api, selection, "peer", progress);
    expect(fn.mock.calls.map((call) => call[0])).toEqual(["new", "directory", "upload", "send"]);
    expect(fn).toHaveBeenNthCalledWith(
      3,
      "upload",
      selection.items[1].file,
      "PUT",
      {
        "X-Xfer-Path": "bundle%2Fhello.txt",
      },
      undefined,
    );
    expect(progress).toHaveBeenCalledWith(5);
  });
});
describe("cancellation", () => {
  it("waits for staging ownership before canceling an interrupted create", async () => {
    const controller = new AbortController();
    const fn = vi.fn(async (path: string) => {
      if (path === "new") controller.abort();
      return {};
    });
    await expect(
      sendSelection(fn as Api, selection, "peer", vi.fn(), controller.signal),
    ).rejects.toThrow();
    expect(fn.mock.calls.map((call) => call[0])).toEqual(["new", "cancel"]);
  });
  it("does not create staging if already canceled", async () => {
    const controller = new AbortController();
    controller.abort();
    const { fn, api } = mockApi();
    await expect(
      sendSelection(api, selection, "peer", vi.fn(), controller.signal),
    ).rejects.toThrow();
    expect(fn).not.toHaveBeenCalled();
  });
});
describe("file selection", () => {
  it("groups multiple files under one portable root", () => {
    const selected = fromFiles([new File([], "a.txt"), new File([], "b.txt")]);
    expect(selected?.items.map((item) => item.path)).toEqual([
      "Shared items/a.txt",
      "Shared items/b.txt",
    ]);
    expect(selected?.folder).toBe(true);
    expect(fromFiles([])).toBeNull();
  });
  it("drains directory batches and retains empty directories", async () => {
    const empty = {
      name: "empty",
      isDirectory: true,
      createReader: () => ({
        readEntries: (resolve: (entries: FileSystemEntry[]) => void) => resolve([]),
      }),
    };
    const file = {
      name: "hello.txt",
      isFile: true,
      file: (resolve: (file: File) => void) => resolve(new File(["hi"], "hello.txt")),
    };
    const batches = [[empty], [file], []];
    const root = {
      name: "folder",
      isDirectory: true,
      createReader: () => ({
        readEntries: (resolve: (entries: unknown[]) => void) => resolve(batches.shift()!),
      }),
    };
    const result = await walk(root as unknown as FileSystemEntry);
    expect(result.map((item) => item.path)).toEqual(["folder", "folder/empty", "folder/hello.txt"]);
    expect(result[1].directory).toBe(true);
  });
});
