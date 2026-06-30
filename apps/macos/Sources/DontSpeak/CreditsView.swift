//  CreditsView.swift
//
//  The Credits tab: the third-party open-source models + runtimes DontSpeak downloads,
//  each with its license. The list is the SAME shared catalog every platform renders —
//  read via the FFI (ds_libraries_json → the shared ds-model `libraries` catalog), already
//  FILTERED to this platform (so an Apple-Silicon build shows the Core ML / ANE model sets
//  and never the CUDA GPU runtime). It can't drift from what's actually fetched.

import SwiftUI
import CDontSpeak

/// One downloaded file of a library, from the catalog's `files` array.
struct LibraryFile: Identifiable, Sendable {
    let name: String
    let url: String
    let sizeBytes: Int?
    var id: String { name }
}

/// One library/project as shown in the list.
struct LibraryInfo: Identifiable, Sendable {
    let name: String
    let usage: String
    let homepage: String
    let license: String
    let licenseURL: String
    let files: [LibraryFile]
    var id: String { name }
}

/// Wire shape of the FFI libraries catalog (`ds_libraries_json` → the shared ds-model
/// `libraries::catalog`): an array of projects, each with a `files` array. Decoded
/// type-safely, then mapped to the view models above.
private struct LibraryDTO: Decodable {
    let name: String
    let usage: String?
    let homepage: String?
    let license: String?
    let license_url: String?
    let files: [LibraryFileDTO]?
}

private struct LibraryFileDTO: Decodable {
    let name: String
    let url: String?
    let size_bytes: Int?
}

/// Read the catalog from the FFI and decode it into typed `LibraryInfo`s. Display order is
/// the catalog's own (lowest-level first), so render as-is — no re-sorting.
private func loadLibraries() -> [LibraryInfo] {
    guard let dtos = ffiDecode([LibraryDTO].self, ds_libraries_json) else { return [] }
    return dtos.map { d in
        LibraryInfo(
            name: d.name,
            usage: d.usage ?? "",
            homepage: d.homepage ?? "",
            license: d.license ?? "",
            licenseURL: d.license_url ?? "",
            files: (d.files ?? []).map {
                LibraryFile(name: $0.name, url: $0.url ?? "", sizeBytes: $0.size_bytes)
            }
        )
    }
}

/// Human file size ("310 MB") from a byte count — file-style (decimal) units, matching the
/// "~310 MB" sizing the download manifest shows.
private func humanSize(_ bytes: Int) -> String {
    let f = ByteCountFormatter()
    f.countStyle = .file
    f.allowsNonnumericFormatting = false
    return f.string(fromByteCount: Int64(bytes))
}

struct CreditsView: View {
    @State private var libraries: [LibraryInfo] = []
    /// Names of the libraries currently expanded (collapsed by default) — same disclosure
    /// idea as the Tools pane: a tappable header with a rotating chevron.
    @State private var expanded: Set<String> = []

    var body: some View {
        // The Libraries pane of the merged sidebar window — just the scrollable content; the
        // glass slab + traffic-light strip live once on the `MainWindow` container.
        libraryList
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // The catalog is immutable for the process lifetime, so load it ONCE — re-navigating
            // to this tab re-fires `onAppear` but must not re-run the FFI + JSON decode.
            .onAppear { if libraries.isEmpty { libraries = loadLibraries() } }
    }

    /// The catalog as a Control-Center / HUD layout matching the Tools pane: one glass slab
    /// with the libraries on a single headerless "platter".
    @ViewBuilder private var libraryList: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Platter {
                    ForEach(Array(libraries.enumerated()), id: \.element.id) { idx, lib in
                        if idx > 0 { PlatterDivider() }
                        libraryRow(lib)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .windowContentInset()
        }
        .scrollIndicators(.hidden)
    }

    /// One collapsible library: a tappable header (just the name + a rotating chevron — the
    /// collapsed row stays clean) that reveals what it's for, the project link, the license (its
    /// name links to the license page), and the files it fetches — the shared `DisclosureRow`,
    /// the same disclosure look the Tools pane uses.
    @ViewBuilder
    private func libraryRow(_ lib: LibraryInfo) -> some View {
        DisclosureRow(expanded: $expanded, id: lib.name) {
            Text(lib.name).glassRowTitle()
        } content: {
            libraryDetail(lib)
        }
    }

    /// The expanded body of a library row: what it's for, its links, then the files it fetches.
    @ViewBuilder
    private func libraryDetail(_ lib: LibraryInfo) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if !lib.usage.isEmpty {
                Text(lib.usage)
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack(spacing: 14) {
                if !lib.homepage.isEmpty, let url = URL(string: lib.homepage) {
                    Link(destination: url) {
                        Label(L.t("libraries.homepage"), systemImage: "link")
                    }
                }
                // The license LINK is labeled with the license name itself (e.g. "MIT",
                // "Apache-2.0") and opens its license page.
                if !lib.license.isEmpty, !lib.licenseURL.isEmpty, let url = URL(string: lib.licenseURL) {
                    Link(destination: url) {
                        Label(lib.license, systemImage: "doc.text")
                    }
                }
            }
            .font(.caption)

            if !lib.files.isEmpty {
                Text(L.t("libraries.files"))
                    .font(.caption2).fontWeight(.semibold)
                    .foregroundStyle(.tertiary).textCase(.uppercase)
                    .padding(.top, 2)
                ForEach(lib.files) { f in
                    fileRow(f)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 14).padding(.vertical, 10)
    }

    @ViewBuilder
    private func fileRow(_ f: LibraryFile) -> some View {
        HStack(spacing: 6) {
            Text(f.name)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(1).truncationMode(.middle)
            Spacer(minLength: 8)
            if let b = f.sizeBytes, b > 0 {
                Text(humanSize(b)).font(.caption2).foregroundStyle(.tertiary)
            }
        }
        .padding(.leading, 10)
        .padding(.vertical, 1)
    }
}
