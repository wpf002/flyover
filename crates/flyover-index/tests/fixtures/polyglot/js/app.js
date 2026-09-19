import { readFile } from "fs";
const path = require("path");

export function greet(name) {
  return "hi " + name + path.sep;
}

export class Service {
  start() {
    readFile("x", () => {});
    return true;
  }
}
