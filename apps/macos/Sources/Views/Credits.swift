// The Acknowledgements and Licences sheets About opens, from the core's credits document
// (the same one the GTK and Windows apps show).

import SwiftUI

struct CreditsDocument: Decodable {
    struct Project: Decodable, Identifiable {
        let name: String, by: String, url: String, licence: String, note: String
        var id: String { name }
    }
    struct Thanks: Decodable, Identifiable {
        let name: String, reason: String, url: String
        var id: String { name }
        enum CodingKeys: String, CodingKey { case name, reason = "for", url }
    }
    struct Photograph: Decodable, Identifiable {
        let title: String, author: String, year: String, licence: String, licenceUrl: String, source: String, changes: String
        var id: String { source }
    }
    struct Artwork: Decodable, Identifiable {
        let name: String, from: String, url: String?
        var id: String { name }
    }
    struct QemuSource: Decodable { let tag: String, label: String, url: String, archiveUrl: String, note: String }
    struct LicenceEntry: Decodable, Identifiable {
        let components: [String], text: String
        var id: String { components.joined(separator: ",") }
    }
    struct Licence: Decodable, Identifiable {
        let name: String, entries: [LicenceEntry]
        var id: String { name }
        var componentCount: Int { entries.reduce(0) { $0 + $1.components.count } }
    }

    let copyright: String, licenceName: String, projectUrl: String, trademark: String
    let builtOn: [Project], thanks: [Thanks], photographs: [Photograph], artwork: [Artwork]
    let qemu: QemuSource
    let licences: [Licence]

    /// The document the core builds; nil if it reports an error.
    static func load() -> CreditsDocument? {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try? decoder.decode(CreditsDocument.self, from: Data(origamiCreditsJson().utf8))
    }
}

private struct CreditLink: Identifiable {
    let label: String, url: String
    var id: String { label + url }
}

private struct CreditRow: View {
    let title: String
    let detail: String
    var links: [CreditLink] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.body.weight(.medium))
            if !detail.isEmpty {
                Text(detail).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
            HStack(spacing: 10) {
                ForEach(links) { l in
                    if let u = URL(string: l.url) { Link(l.label, destination: u).font(.caption) }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 2)
    }
}

/// What Origami is built on, the thanks, the photographs and the artwork.
struct AcknowledgementsView: View {
    @Environment(\.dismiss) private var dismiss
    private let doc = CreditsDocument.load()

    var body: some View {
        VStack(spacing: 0) {
            if let c = doc {
                Form {
                    Section("Built on") {
                        ForEach(c.builtOn) { p in
                            CreditRow(title: p.name, detail: "\(p.by). \(p.note)", links: [CreditLink(label: "Website", url: p.url)])
                            if p.name.hasPrefix("QEMU") {
                                CreditRow(title: c.qemu.label, detail: "The exact source of the bundled build.",
                                          links: [CreditLink(label: "Source", url: c.qemu.url)])
                            }
                        }
                    }
                    Section("Thanks") {
                        ForEach(c.thanks) { t in
                            CreditRow(title: t.name, detail: t.reason, links: [CreditLink(label: "Website", url: t.url)])
                        }
                    }
                    Section("Photographs") {
                        ForEach(c.photographs) { p in
                            CreditRow(title: p.title, detail: "\(p.author), \(p.year). \(p.changes).",
                                      links: [CreditLink(label: p.licence, url: p.licenceUrl), CreditLink(label: "Source", url: p.source)])
                        }
                    }
                    Section("Artwork") {
                        ForEach(c.artwork) { a in
                            CreditRow(title: a.name, detail: a.from,
                                      links: a.url.map { [CreditLink(label: "Source", url: $0)] } ?? [])
                        }
                        Text(c.trademark).font(.caption).foregroundStyle(.secondary)
                    }
                }
                .formStyle(.grouped)
            } else {
                Text("The credits could not be read.").padding()
                Spacer()
            }
            HStack { Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }.padding(12)
        }
        .frame(width: 480, height: 560)
    }
}

/// The full text of every bundled licence, grouped, with a filter.
struct LicencesView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var open: Set<String> = []
    private let doc = CreditsDocument.load()

    private func matches(_ l: CreditsDocument.Licence, _ e: CreditsDocument.LicenceEntry) -> Bool {
        query.isEmpty || l.name.localizedCaseInsensitiveContains(query)
            || e.components.contains { $0.localizedCaseInsensitiveContains(query) }
    }

    private func expanded(_ key: String, forced: Bool) -> Binding<Bool> {
        Binding(get: { forced || open.contains(key) },
                set: { if $0 { open.insert(key) } else { open.remove(key) } })
    }

    private func title(_ components: [String]) -> String {
        var out = ""
        for (i, c) in components.enumerated() {
            if i > 0 && out.count + c.count > 60 { return out + " and \(components.count - i) more" }
            out += (i > 0 ? ", " : "") + c
        }
        return out
    }

    @ViewBuilder private func group(_ l: CreditsDocument.Licence) -> some View {
        let shown = l.entries.filter { matches(l, $0) }
        if !shown.isEmpty {
            DisclosureGroup(isExpanded: expanded(l.id, forced: !query.isEmpty)) {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(shown) { e in
                        DisclosureGroup(isExpanded: expanded(l.id + "/" + e.id, forced: false)) {
                            VStack(alignment: .leading, spacing: 6) {
                                if e.components.count > 1 {
                                    Text(e.components.joined(separator: ", ")).font(.caption).foregroundStyle(.secondary)
                                }
                                Text(e.text).font(.system(.caption, design: .monospaced))
                            }
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 4)
                        } label: {
                            Text(title(e.components))
                        }
                    }
                }
                .padding(.leading, 12)
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text(l.name).font(.body.weight(.medium))
                    Text(l.componentCount == 1 ? "1 component" : "\(l.componentCount) components")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Licences").font(.headline)
            if let c = doc {
                TextField("Search components and licences", text: $query).textFieldStyle(.roundedBorder)
                Text(c.qemu.note).font(.caption).foregroundStyle(.secondary)
                if let u = URL(string: c.qemu.url) { Link("Source of \(c.qemu.label)", destination: u).font(.caption) }
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        ForEach(c.licences) { group($0) }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                Text("The licences could not be read.")
                Spacer()
            }
            HStack { Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }
        }
        .padding(16).frame(width: 520, height: 600)
    }
}
