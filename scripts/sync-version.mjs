#!/usr/bin/env node
// Fan the package.json version (bumped by `changeset version`) out to every
// other file that must agree with it: the Tauri config the release workflow
// verifies the tag against, the Rust crate manifest, and that crate's own
// Cargo.lock entry. Pure JSON/string edits so this runs in the versioning
// workflow without a Rust toolchain.
import { readFileSync, writeFileSync } from "node:fs";

const root = new URL("../", import.meta.url);
const read = (p) => readFileSync(new URL(p, root), "utf8");
const write = (p, c) => writeFileSync(new URL(p, root), c);

const { version } = JSON.parse(read("package.json"));
if (!version) throw new Error("no version found in package.json");

// tauri.conf.json — JSON, prettier-formatted (2-space, trailing newline).
const tauriPath = "src-tauri/tauri.conf.json";
const tauri = JSON.parse(read(tauriPath));
tauri.version = version;
write(tauriPath, JSON.stringify(tauri, null, 2) + "\n");

// Cargo.toml — the [package] version is the first top-level `version = "..."`
// (dependency versions are inline inside `{ ... }`, so the anchored match
// only ever hits the package line).
const cargoPath = "src-tauri/Cargo.toml";
const cargoRe = /^version = "[^"]*"/m;
const cargo = read(cargoPath);
if (!cargoRe.test(cargo)) throw new Error(`no [package] version found in ${cargoPath}`);
write(cargoPath, cargo.replace(cargoRe, `version = "${version}"`));

// Cargo.lock — the farthing crate's own entry.
const lockPath = "src-tauri/Cargo.lock";
const lockRe = /(name = "farthing"\nversion = ")[^"]*(")/;
const lock = read(lockPath);
if (!lockRe.test(lock)) throw new Error(`could not find farthing entry in ${lockPath}`);
write(lockPath, lock.replace(lockRe, `$1${version}$2`));

console.log(`Synced version ${version} → tauri.conf.json, Cargo.toml, Cargo.lock`);
