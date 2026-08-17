package main

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"io/fs"
	"sort"
)

// bundleFingerprint is a hash over the embedded web bundle.
//
// # Why a binary should be able to name what it is carrying
//
// The window ships the interface inside itself, which is the right shape —
// one file to install, nothing to serve — and it means "which build am I
// looking at" has two halves: the Go version stamped by the linker, and the
// bundle, which is built by a different toolchain and could be stale without
// anything saying so. A rebuilt window carrying yesterday's `dist` looks
// exactly like a window whose fix did not work.
//
// So it is computed rather than stamped: names and bytes, in a fixed order,
// hashed. A stamp can be wrong; a hash of the thing cannot.
func bundleFingerprint() (string, error) {
	var names []string
	err := fs.WalkDir(bundle, ".", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !d.IsDir() {
			names = append(names, path)
		}
		return nil
	})
	if err != nil {
		return "", fmt.Errorf("reading the bundle: %w", err)
	}
	// Sorted, because `WalkDir` is lexical per directory and this has to be
	// the same number on two machines that built the same bundle.
	sort.Strings(names)

	sum := sha256.New()
	for _, name := range names {
		// The name is hashed as well as the contents: a file *renamed* is a
		// different bundle, and hashing only contents would call it the same.
		_, _ = fmt.Fprintf(sum, "%s\x00", name)
		file, openErr := bundle.Open(name)
		if openErr != nil {
			return "", fmt.Errorf("reading %s: %w", name, openErr)
		}
		_, copyErr := io.Copy(sum, file)
		_ = file.Close()
		if copyErr != nil {
			return "", fmt.Errorf("reading %s: %w", name, copyErr)
		}
	}
	return hex.EncodeToString(sum.Sum(nil))[:16], nil
}

// bundleFiles is how many files are embedded, for the version line.
func bundleFiles() int {
	n := 0
	_ = fs.WalkDir(bundle, ".", func(_ string, d fs.DirEntry, err error) error {
		if err == nil && !d.IsDir() {
			n++
		}
		return nil
	})
	return n
}
