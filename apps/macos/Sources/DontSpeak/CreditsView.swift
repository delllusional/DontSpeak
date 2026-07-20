// Libraries/Credits: third-party models + runtimes from `ds_libraries_json`
// (shared ds-model catalog, platform-filtered — no CUDA on Apple Silicon).

import CDontSpeak
import SwiftUI

struct LibraryFile: Identifiable, Sendable {
    let name: String
    let url: String
    let sizeBytes: Int?
    var id: String { name }
}

struct LibraryInfo: Identifiable, Sendable {
    let name: String
    let usage: String
    let homepage: String
    let license: String
    let licenseURL: String
    let languages: [String]
    let languageCount: Int?
    let automaticLanguageDetection: Bool
    let languageListURL: String
    let files: [LibraryFile]
    var id: String { name }
}

private struct LibraryDTO: Decodable {
    let name: String
    let usage: String?
    let homepage: String?
    let license: String?
    let licenseURL: String?
    let languages: [String]?
    let languageCount: Int?
    let automaticLanguageDetection: Bool?
    let languageListURL: String?
    let files: [LibraryFileDTO]?

    enum CodingKeys: String, CodingKey {
        case name, usage, homepage, license, files
        case licenseURL = "license_url"
        case languages
        case languageCount = "language_count"
        case automaticLanguageDetection = "automatic_language_detection"
        case languageListURL = "language_list_url"
    }
}

private struct LibraryFileDTO: Decodable {
    let name: String
    let url: String?
    let sizeBytes: Int?

    enum CodingKeys: String, CodingKey {
        case name, url
        case sizeBytes = "size_bytes"
    }
}

/// Catalog order is intentional (lowest-level first) — render as-is.
private func loadLibraries() -> [LibraryInfo] {
    guard let dtos = ffiDecode([LibraryDTO].self, ds_libraries_json) else { return [] }
    return dtos.map { d in
        LibraryInfo(
            name: d.name,
            usage: d.usage ?? "",
            homepage: d.homepage ?? "",
            license: d.license ?? "",
            licenseURL: d.licenseURL ?? "",
            languages: d.languages ?? [],
            languageCount: d.languageCount,
            automaticLanguageDetection: d.automaticLanguageDetection ?? false,
            languageListURL: d.languageListURL ?? "",
            files: (d.files ?? []).map {
                LibraryFile(name: $0.name, url: $0.url ?? "", sizeBytes: $0.sizeBytes)
            }
        )
    }
}

/// Shared Rust formatter (`ds_human_size`, decimal) so every platform's size labels agree.
private func humanSize(_ bytes: Int) -> String {
    guard let ptr = ds_human_size(UInt64(max(0, bytes))) else { return "" }
    defer { ds_string_free(ptr) }
    return String(cString: ptr)
}

struct CreditsView: View {
    @State private var libraries: [LibraryInfo] = []
    @State private var expanded: Set<String> = []

    var body: some View {
        libraryList
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // Catalog is process-immutable; re-navigating re-fires onAppear — load once.
            .onAppear { if libraries.isEmpty { libraries = loadLibraries() } }
    }

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

    @ViewBuilder
    private func libraryRow(_ lib: LibraryInfo) -> some View {
        DisclosureRow(expanded: $expanded, id: lib.name) {
            Text(lib.name).glassRowTitle()
        } content: {
            libraryDetail(lib)
        }
    }

    @ViewBuilder
    private func libraryDetail(_ lib: LibraryInfo) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if !lib.usage.isEmpty {
                Text(lib.usage)
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if !lib.languages.isEmpty || lib.languageCount != nil {
                Text(L.t("libraries.languages"))
                    .font(.caption2).fontWeight(.semibold)
                    .foregroundStyle(.tertiary).textCase(.uppercase)
                    .padding(.top, 2)
                Text(languageSummary(lib))
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if !lib.languageListURL.isEmpty, let url = URL(string: lib.languageListURL) {
                    Link(destination: url) {
                        Label(L.t("libraries.full_language_list"), systemImage: "globe")
                    }
                    .font(.caption)
                }
            }

            HStack(spacing: 14) {
                if !lib.homepage.isEmpty, let url = URL(string: lib.homepage) {
                    Link(destination: url) {
                        Label(L.t("libraries.homepage"), systemImage: "link")
                    }
                }
                // Link label is the SPDX/name itself (e.g. "MIT").
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

    private func languageSummary(_ lib: LibraryInfo) -> String {
        if lib.automaticLanguageDetection, let count = lib.languageCount {
            return L.t("libraries.automatic_languages", ["count": String(count)])
        }
        return lib.languages
            .filter { $0 != "auto" }
            .map { L.t("language.\($0)") }
            .joined(separator: ", ")
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
