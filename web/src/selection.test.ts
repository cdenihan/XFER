import { expect, test } from "vite-plus/test";
import { fromDrop } from "./selection";

test("file drops fall back when entry APIs are absent or return null", async () => {
  const file = new File(["payload"], "dropped.txt");
  const data = {
    items: [{}, { webkitGetAsEntry: () => null }],
    files: [file],
  } as unknown as DataTransfer;
  expect(await fromDrop(data)).toEqual({
    items: [{ file, path: "dropped.txt" }],
    root: "dropped.txt",
    folder: false,
  });
});
