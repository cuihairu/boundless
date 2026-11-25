//! `resourcechecker` port (`tools/resourcechecker/visual_checker.*`): the
//! `VisualChecker` that judges a `.visual` against the per-directory rules
//! in `visual_rules.xml`.
//!
//! Fully portable faces: the rule table walk (matching rules sorted by
//! directory-name length, `PathMatchSpec`-style file patterns, `parent`
//! inheritance for every setting), the size / triangle-count / hierarchy
//! / portal / hard-point / texture-name checks with their message-exact
//! diagnostics, and the `errorText` one-error-per-line face.
//!
//! Seams (documented at each site): the Windows `MessageBox` warnings are
//! the exporter UI — the port records the same text through the error
//! list (the reference's `!automatedTest` branch); `PathMatchSpec`
//! (Shlwapi) is reimplemented as a portable glob (`*`, `?`, `[seq]` —
//! the `{}` match-list form has no user in the shipped rules and is not
//! implemented); a visual without a `node` section crashes the reference
//! (`maxNodeDepth` dereferences the null `openSection` result) while the
//! port reports depth 0; an XML file is never a primitives container, so
//! the port declines such files outright instead of casting their empty
//! binary blocks to `Moo::IndexHeader` like the reference does.
//!
//! `snap_vertices_` is carried (the exporter toggles it) but the checks
//! here never read it, matching the reference.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use foundation::math::{BoundBox, Matrix, PlaneEq, Vector3};
use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::{self, BwResource};
use resources::datasection::DataSection;

use crate::primitive_file_structs::IndexHeader;

/// `UNKNOWN_TYPE_NAME`.
pub const UNKNOWN_TYPE_NAME: &str = "(unknown)";

/// The directory paths whose visuals skip every check (collision shells).
const BSP_NAME_TOKEN: &str = "_bsp.";

/// `toLower`: ASCII only, like the reference loop.
fn to_lower(s: &str) -> String {
    s.chars().map(|c| c.to_ascii_lowercase()).collect()
}

/// `PathMatchSpec` (Shlwapi): the MS-DOS wildcards `*` and `?` plus
/// `[seq]`/`[!seq]` classes. Both sides arrive lowercased from the rule
/// walk, so the match is a plain byte compare.
pub fn path_match_spec(name: &str, spec: &str) -> bool {
    fn class_matches(items: &[u8], c: u8) -> bool {
        let mut index = 0;
        while index < items.len() {
            // A `a-z` range runs to the next `-` that is not the last char.
            if index + 2 < items.len() && items[index + 1] == b'-' {
                if items[index] <= c && c <= items[index + 2] {
                    return true;
                }
                index += 3;
            } else {
                if items[index] == c {
                    return true;
                }
                index += 1;
            }
        }
        false
    }

    fn inner(name: &[u8], spec: &[u8]) -> bool {
        match spec.first() {
            None => name.is_empty(),
            Some(b'*') => {
                let mut rest = 1;
                while rest < spec.len() && spec[rest] == b'*' {
                    rest += 1;
                }
                (0..=name.len()).any(|skip| inner(&name[skip..], &spec[rest..]))
            }
            Some(b'?') => !name.is_empty() && inner(&name[1..], &spec[1..]),
            Some(b'[') => {
                // Scan the class; an unterminated one matches a literal `[`.
                let mut end = 1;
                if end < spec.len() && (spec[end] == b'!' || spec[end] == b'^') {
                    end += 1;
                }
                if end < spec.len() && spec[end] == b']' {
                    end += 1; // a leading `]` is a literal
                }
                while end < spec.len() && spec[end] != b']' {
                    end += 1;
                }
                if end >= spec.len() {
                    return name.first() == Some(&b'[') && inner(&name[1..], &spec[1..]);
                }
                let class = &spec[1..end];
                let rest = &spec[end + 1..];
                let Some(&c) = name.first() else {
                    return false;
                };
                let negated = class.first() == Some(&b'!') || class.first() == Some(&b'^');
                let items = if negated { &class[1..] } else { class };
                class_matches(items, c) != negated && inner(&name[1..], rest)
            }
            Some(&expected) => name.first() == Some(&expected) && inner(&name[1..], &spec[1..]),
        }
    }

    inner(name.as_bytes(), spec.as_bytes())
}

/// `ruleSectionCompare`: longer directory names first, ties broken by the
/// longer `filespec` (`std::stable_sort` keeps the file order within a
/// tie).
fn rule_section_compare(a: &DataSection, b: &DataSection) -> std::cmp::Ordering {
    let a_path = a.read_string("path", "").len();
    let b_path = b.read_string("path", "").len();
    if a_path == b_path {
        let a_spec = a.read_string("filespec", "").len();
        let b_spec = b.read_string("filespec", "").len();
        b_spec.cmp(&a_spec)
    } else {
        b_path.cmp(&a_path)
    }
}

macro_rules! recursive_reader {
    ($name:ident, $out:ty, $read:expr) => {
        fn $name(
            key: &str,
            rules_by_id: &BTreeMap<String, DataSection>,
            section: &DataSection,
            default: $out,
        ) -> $out {
            if let Some(ds) = section.open_section(key) {
                let read: fn(&DataSection) -> $out = $read;
                return read(ds);
            }
            let parent = section.read_string("parent", "");
            if let Some(parent) = rules_by_id.get(&parent) {
                return $name(key, rules_by_id, parent, default);
            }
            default
        }
    };
}

recursive_reader!(recursive_read_vector3, Vector3, |ds: &DataSection| ds
    .as_vector3(Vector3::ZERO));
recursive_reader!(recursive_read_string, String, |ds: &DataSection| ds
    .as_string()
    .to_string());
recursive_reader!(recursive_read_int, i32, |ds: &DataSection| ds.as_int(0));
recursive_reader!(recursive_read_float, f32, |ds: &DataSection| ds
    .as_float(0.0));
recursive_reader!(recursive_read_bool, bool, |ds: &DataSection| ds
    .as_bool(false));

/// `recursiveReadStrings`: every `hardPoint` value down the parent chain.
fn recursive_read_strings(
    key: &str,
    rules_by_id: &BTreeMap<String, DataSection>,
    section: &DataSection,
    results: &mut BTreeSet<String>,
) {
    for index in 0..section.count_children() {
        if let Some(child) = section.open_child(index)
            && child.section_name() == key
        {
            results.insert(child.as_string().to_string());
        }
    }
    let parent = section.read_string("parent", "");
    if let Some(parent) = rules_by_id.get(&parent) {
        recursive_read_strings(key, rules_by_id, parent, results);
    }
}

/// The rule children of `section` (`openSections(tag, rules)`).
fn open_sections<'a>(section: &'a DataSection, tag: &str) -> Vec<&'a DataSection> {
    (0..section.count_children())
        .filter_map(|index| section.open_child(index))
        .filter(|child| child.section_name() == tag)
        .collect()
}

/// The `point` children of a portal, as Vector3s
/// (`portal->readVector3s("point", points)`).
fn read_vector3s(section: &DataSection, tag: &str) -> Vec<Vector3> {
    open_sections(section, tag)
        .into_iter()
        .map(|child| child.as_vector3(Vector3::ZERO))
        .collect()
}

// ----------------------------------------------------------------------
// The check helpers.
// ----------------------------------------------------------------------

/// `snapValue`: snap a scalar onto a multiple of `snap_size`.
fn snap_value(v: &mut f32, snap_size: f32) {
    if snap_size > 0.0 {
        let half_snap = snap_size / 2.0;
        if *v > 0.0 {
            *v += half_snap;
            *v -= *v % snap_size;
        } else {
            *v -= half_snap;
            *v += (-*v) % snap_size;
        }
    }
}

/// `snapVector3`.
fn snap_vector3(vec: &Vector3, snaps: &Vector3) -> Vector3 {
    let mut v = *vec;
    if snaps.x != 0.0 {
        snap_value(&mut v.x, snaps.x);
    }
    if snaps.y != 0.0 {
        snap_value(&mut v.y, snaps.y);
    }
    if snaps.z != 0.0 {
        snap_value(&mut v.z, snaps.z);
    }
    v
}

/// `isMultipleOf` with the `1/8192` default epsilon.
fn is_multiple_of(v: f32, m: f32) -> bool {
    is_multiple_of_eps(v, m, 1.0 / 8192.0)
}

/// `isMultipleOf(v, m, epsilon)`.
fn is_multiple_of_eps(v: f32, m: f32, epsilon: f32) -> bool {
    let diff = (v % m).abs();
    diff < epsilon || diff - m > -epsilon
}

/// `close`.
fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1.0 / 8192.0
}

/// `checkDuplicateNodeNames`: recursive `identifier` collection with the
/// duplicates set apart.
fn check_duplicate_node_names(
    node_section: Option<&DataSection>,
    node_names: &mut BTreeSet<String>,
    duplicates: &mut BTreeSet<String>,
) {
    let Some(node_section) = node_section else {
        return;
    };
    let id = node_section.read_string("identifier", "");
    if !node_names.insert(id.clone()) {
        duplicates.insert(id);
    }
    for node in open_sections(node_section, "node") {
        check_duplicate_node_names(Some(node), node_names, duplicates);
    }
}

/// `extractHardPoints`: every `HP_*` identifier down the node tree.
fn extract_hard_points(node_section: Option<&DataSection>, hard_points: &mut BTreeSet<String>) {
    let Some(node_section) = node_section else {
        return;
    };
    let id = node_section.read_string("identifier", "");
    if id.len() > 3 && &id[..3] == "HP_" {
        hard_points.insert(id);
    }
    for node in open_sections(node_section, "node") {
        extract_hard_points(Some(node), hard_points);
    }
}

/// `maxNodeDepth`: the deepest `node` tree plus one.
///
/// Registered divergence: the reference passes the possibly-null
/// `openSection("node")` result straight in and would crash on a
/// nodeless visual; the port reports 0.
fn max_node_depth(node_section: Option<&DataSection>) -> u32 {
    let Some(node_section) = node_section else {
        return 0;
    };
    let mut max_depth = 0;
    for node in open_sections(node_section, "node") {
        max_depth = max_node_depth(Some(node)).max(max_depth);
    }
    max_depth + 1
}

/// `PrimitiveFile::get`: the `.processed` container first, then the bare
/// resource, handed over as the binary section tree the triangle walk
/// needs. An XML file is never a primitives container, so it declines
/// (the reference would cast empty binary blocks into an `IndexHeader`).
fn primitives_file(resource: &BwResource, prim_res_name: &str) -> Option<BinSection> {
    for candidate in [
        format!("{prim_res_name}.processed"),
        prim_res_name.to_string(),
    ] {
        let Some(raw) = resource.file_system().read_file(&candidate) else {
            continue;
        };
        let looks_xml = raw
            .iter()
            .find(|&&b| b != b' ' && b != b'\t' && b != b'\r' && b != b'\n')
            == Some(&b'<');
        if !looks_xml {
            let mut section = BinSection::new(&candidate, BinaryBlock::new(raw));
            section.introspect();
            return Some(section);
        }
    }
    None
}

// ----------------------------------------------------------------------
// VisualChecker.
// ----------------------------------------------------------------------

/// Ensures that a given visual passes a series of validity checks
/// (`VisualChecker`).
pub struct VisualChecker {
    min_size: Vector3,
    max_size: Vector3,
    max_triangles: u32,
    min_triangles: u32,
    max_hierarchy_depth: u32,
    // The exporter toggles this, but the checks never read it — dead in
    // the reference too (`snapVertices_`).
    #[allow(dead_code)]
    snap_vertices: bool,
    portals: bool,
    portal_snap: Vector3,
    portal_distance: f32,
    portal_offset: f32,
    check_hard_points: bool,
    hard_points: BTreeSet<String>,

    type_name: String,
    export_as: String,

    errors: Vec<String>,
}

impl VisualChecker {
    /// `VisualChecker(visualName)` — cached rules, snapping on.
    pub fn new(resource: &BwResource, visual_name: &str) -> Self {
        Self::with_options(resource, visual_name, true, true)
    }

    /// `VisualChecker(visualName, cacheRules, snapVertices)`.
    pub fn with_options(
        resource: &BwResource,
        visual_name: &str,
        cache_rules: bool,
        snap_vertices: bool,
    ) -> Self {
        let mut checker = Self {
            min_size: Vector3::ZERO,
            max_size: Vector3::ZERO,
            max_triangles: 0,
            min_triangles: 0,
            max_hierarchy_depth: 0,
            snap_vertices,
            portals: false,
            portal_snap: Vector3::ZERO,
            portal_distance: 0.0,
            portal_offset: 0.0,
            check_hard_points: false,
            hard_points: BTreeSet::new(),
            type_name: UNKNOWN_TYPE_NAME.to_string(),
            export_as: "normal".to_string(),
            errors: Vec::new(),
        };

        // Do no checking for collision visuals.
        if visual_name.contains(BSP_NAME_TOKEN) {
            return checker;
        }

        let rules_section = if cache_rules || resource.file_exists("visual_rules.xml") {
            resource.open_section("visual_rules.xml", false)
        } else {
            None
        };

        // No rules, allow everything.
        let Some(rules_section) = rules_section else {
            // The reference shows a MessageBox unless running under an
            // automated test; the port always takes the record path.
            checker.add_error(
                "VisualChecker::VisualChecker - Unable to find \
                 visual_rules.xml file.",
            );
            return checker;
        };

        // Get all matching rules, sorted by directory length.
        let visual_path = bwresource::get_file_path(visual_name);
        let visual_filename = bwresource::get_filename(visual_name).to_string();

        let rules: Vec<DataSection> = open_sections(&rules_section, "rule")
            .into_iter()
            .cloned()
            .collect();
        let mut matching_rules: Vec<&DataSection> = rules
            .iter()
            .filter(|rule| {
                let rule_path = to_lower(&rule.read_string("path", ""));
                visual_path.contains(&rule_path)
            })
            .collect();
        matching_rules.sort_by(|a, b| rule_section_compare(a, b));

        // Traverse down the list until we find a file pattern that matches.
        let rule_section = matching_rules.into_iter().find(|rule| {
            let file_pattern = to_lower(&rule.read_string("filespec", ""));
            // Matches everything, we've found something.
            file_pattern.is_empty() || path_match_spec(&visual_filename, &file_pattern)
        });

        // No matching rule section, bail.
        let Some(rule_section) = rule_section else {
            return checker;
        };

        // Build a map of rule id -> rule section.
        let mut rules_by_id: BTreeMap<String, DataSection> = BTreeMap::new();
        for rule in &rules {
            let id = rule.read_string("identifier", "");
            if !id.is_empty() {
                rules_by_id.insert(id, rule.clone());
            }
        }

        checker.type_name = rule_section.read_string("identifier", "");
        checker.export_as = recursive_read_string(
            "exportAs",
            &rules_by_id,
            rule_section,
            checker.export_as.clone(),
        );
        if !(checker.export_as == "normal"
            || checker.export_as == "static"
            || checker.export_as == "static with nodes")
        {
            checker.add_error(&format!(
                "Unknown value for exportAs [{}]\n",
                checker.export_as
            ));
            checker.export_as = "normal".to_string();
        }

        checker.min_size =
            recursive_read_vector3("minSize", &rules_by_id, rule_section, checker.min_size);
        checker.max_size =
            recursive_read_vector3("maxSize", &rules_by_id, rule_section, checker.max_size);
        checker.min_triangles = recursive_read_int(
            "minTriangles",
            &rules_by_id,
            rule_section,
            checker.min_triangles as i32,
        ) as u32;
        checker.max_triangles = recursive_read_int(
            "maxTriangles",
            &rules_by_id,
            rule_section,
            checker.max_triangles as i32,
        ) as u32;
        checker.max_hierarchy_depth = recursive_read_int(
            "maxHierarchyDepth",
            &rules_by_id,
            rule_section,
            checker.max_hierarchy_depth as i32,
        ) as u32;
        checker.portals =
            recursive_read_bool("portals", &rules_by_id, rule_section, checker.portals);
        checker.portal_snap = recursive_read_vector3(
            "portalSnap",
            &rules_by_id,
            rule_section,
            checker.portal_snap,
        );
        checker.portal_distance = recursive_read_float(
            "portalDistance",
            &rules_by_id,
            rule_section,
            checker.portal_distance,
        );
        checker.portal_offset = recursive_read_float(
            "portalOffset",
            &rules_by_id,
            rule_section,
            checker.portal_offset,
        );
        checker.check_hard_points = recursive_read_bool(
            "checkUnknownHardPoints",
            &rules_by_id,
            rule_section,
            checker.check_hard_points,
        );

        recursive_read_strings(
            "hardPoint",
            &rules_by_id,
            rule_section,
            &mut checker.hard_points,
        );
        checker
    }

    /// Check the visual for errors according to our current rules. The
    /// `visual_section` is the loaded `.visual`; `prim_res_name` names the
    /// primitives container (its `.processed` twin wins).
    pub fn check(
        &mut self,
        resource: &BwResource,
        visual_section: Option<&DataSection>,
        prim_res_name: &str,
    ) -> bool {
        self.errors.clear();

        let Some(visual_section) = visual_section else {
            self.add_error("Invalid visual, null DataSection");
            return false;
        };

        let Some(mut prims) = primitives_file(resource, prim_res_name) else {
            self.add_error("Invalid primitive file, file not found");
            return false;
        };

        self.run_checks(resource, visual_section, &mut prims)
    }

    fn run_checks(
        &mut self,
        _resource: &BwResource,
        visual_section: &DataSection,
        prims: &mut BinSection,
    ) -> bool {
        let mut good = true;

        let bb_min = visual_section.read_vector3("boundingBox/min", Vector3::ZERO);
        let bb_max = visual_section.read_vector3("boundingBox/max", Vector3::ZERO);
        let size = bb_max - bb_min;

        // Check it's bigger than min size.
        if (self.min_size.x > 0.0 && self.min_size.x > size.x)
            || (self.min_size.y > 0.0 && self.min_size.y > size.y)
            || (self.min_size.z > 0.0 && self.min_size.z > size.z)
        {
            good = false;
            self.add_error(&format!(
                "Too small, must be at least {:.6}, {:.6}, {:.6}",
                self.min_size.x, self.min_size.y, self.min_size.z
            ));
        }

        // Check it's smaller than max size.
        if (self.max_size.x > 0.0 && self.max_size.x < size.x)
            || (self.max_size.y > 0.0 && self.max_size.y < size.y)
            || (self.max_size.z > 0.0 && self.max_size.z < size.z)
        {
            good = false;
            self.add_error(&format!(
                "Too big, must be no bigger than {:.6}, {:.6}, {:.6}",
                self.max_size.x, self.max_size.y, self.max_size.z
            ));
        }

        // Check that it doesn't have too many triangles.
        if !self.check_triangle_count(prims) {
            // error added in check_triangle_count method.
            good = false;
        }

        let hierarchy_depth = max_node_depth(visual_section.open_section("node"));

        if self.max_hierarchy_depth != 0 && hierarchy_depth > self.max_hierarchy_depth {
            good = false;
            self.add_error(&format!(
                "Node hierarchy too deep, depth must be {0} nodes or less",
                self.max_hierarchy_depth
            ));
        }

        // Check that its portals are correct.
        let boundaries = open_sections(visual_section, "boundary");
        let mut portals: Vec<(&DataSection, &DataSection)> = Vec::new();
        for boundary in &boundaries {
            for portal in open_sections(boundary, "portal") {
                portals.push((boundary, portal));
            }
        }

        if self.portals && portals.is_empty() {
            good = false;
            self.add_error("Must have one or more portals");
        }
        if !self.portals && !portals.is_empty() {
            good = false;
            self.add_error("Must not have any portals");
        }

        if self.portals {
            for (boundary, portal) in &portals {
                self.check_portal(boundary, portal, &bb_min, &bb_max, &mut good);
            }
        }

        let mut visual_nodes = BTreeSet::new();
        let mut node_duplicates = BTreeSet::new();
        check_duplicate_node_names(
            visual_section.open_section("node"),
            &mut visual_nodes,
            &mut node_duplicates,
        );
        for name in &node_duplicates {
            good = false;
            self.add_error(&format!("Duplicate node name {name}"));
        }

        // Check that its hard points are correct.
        let mut visual_hard_points = BTreeSet::new();
        extract_hard_points(visual_section.open_section("node"), &mut visual_hard_points);

        // Check we have all the hard points we need to.
        let missing: Vec<String> = self
            .hard_points
            .iter()
            .filter(|wanted| !visual_hard_points.contains(*wanted))
            .cloned()
            .collect();
        for wanted in &missing {
            good = false;
            self.add_error(&format!("Missing hard point {wanted}"));
        }

        // Check we don't have any hard points we shouldn't.
        if self.check_hard_points {
            for found in &visual_hard_points {
                if !self.hard_points.contains(found) {
                    // Check for a well formed flash HP
                    // Must be along the lines of HP_flash01
                    let bytes = found.as_bytes();
                    if bytes.len() == 10
                        && &found[..8] == "HP_flash"
                        && bytes[8].is_ascii_digit()
                        && bytes[9].is_ascii_digit()
                    {
                        continue;
                    }

                    // HP_flash is also a valid flash hardpoint name
                    if found == "HP_flash" {
                        continue;
                    }

                    good = false;
                    self.add_error(&format!("Unknown hard point {found}"));
                }
            }
        }

        // Check that no texture name has a space in it.
        for render_set in open_sections(visual_section, "renderSet") {
            for geometry in open_sections(render_set, "geometry") {
                for group in open_sections(geometry, "primitiveGroup") {
                    let texture_name = group.read_string("material/textureormfm", "");
                    if texture_name.rfind(' ').is_some() {
                        good = false;
                        self.add_error(&format!(
                            "Illegal texture name \"{texture_name}\", no \
                             spaces in name or path allowed\n"
                        ));
                    }
                }
            }
        }

        good
    }

    /// One portal's battery of checks (the `if (portals_)` loop body).
    fn check_portal(
        &mut self,
        boundary: &DataSection,
        portal: &DataSection,
        bb_min: &Vector3,
        bb_max: &Vector3,
        good: &mut bool,
    ) {
        // Check that the portal is alright.
        let mut points = read_vector3s(portal, "point");

        // Portal points get mapped back into object space, from plane
        // space.
        let normal = boundary.read_vector3("normal", Vector3::ZERO);
        let mut basis = Matrix::zero();
        let u_axis = portal.read_vector3("uAxis", Vector3::ZERO);
        basis.m[0][0] = u_axis.x;
        basis.m[0][1] = u_axis.y;
        basis.m[0][2] = u_axis.z;
        basis.m[2][0] = normal.x;
        basis.m[2][1] = normal.y;
        basis.m[2][2] = normal.z;
        let row1 = normal.cross_product(&normal, &u_axis);
        basis.m[1][0] = row1.x;
        basis.m[1][1] = row1.y;
        basis.m[1][2] = row1.z;
        let d = boundary.read_float("d", 0.0);
        basis.translation(&(normal * d));
        for pt in &mut points {
            *pt = basis.apply_point(pt);
        }

        // Check the portal is valid.
        if points.len() < 3 {
            *good = false;
            self.add_error("Not enough points in a portal");
            return;
        }

        let mut portal_centre = Vector3::ZERO;
        for point in &points {
            portal_centre = portal_centre + *point;
        }
        portal_centre = portal_centre * (1.0 / points.len() as f32);

        let mut plane = PlaneEq::default();
        let mut has_plane_eq = false;
        for index in 0..points.len().saturating_sub(2) {
            plane.init(&points[index + 2], &points[index + 1], &points[index]);

            let normal = plane.normal();
            if !(normal.x == 0.0 && normal.y == 0.0 && normal.z == 0.0) {
                has_plane_eq = true;
                break;
            }
        }

        if !has_plane_eq {
            *good = false;
            self.add_error("Portal is non planar");
            return;
        }

        let bb = BoundBox::new(*bb_min, *bb_max);
        if plane.d() - bb.centre().dot_product(plane.normal()) > 0.0 {
            *good = false;
            self.add_error("Portal is facing outwards");
            return;
        }

        // Check it's snapped correctly.
        let mut portal_bb = BoundBox::default();
        for point in &points {
            portal_bb.add_bounds(point);
        }

        let min_diff = snap_vector3(&portal_bb.min, &self.portal_snap) - portal_bb.min;
        let max_diff = snap_vector3(&portal_bb.max, &self.portal_snap) - portal_bb.max;

        const ERROR_EPSILON: f32 = 1.0 / 2196.0;
        if min_diff.x.abs() > ERROR_EPSILON
            || min_diff.y.abs() > ERROR_EPSILON
            || min_diff.z.abs() > ERROR_EPSILON
            || max_diff.x.abs() > ERROR_EPSILON
            || max_diff.y.abs() > ERROR_EPSILON
            || max_diff.z.abs() > ERROR_EPSILON
        {
            *good = false;
            self.add_error(&format!(
                "Portals must reside on multiples of {:.6}, {:.6}, {:.6}",
                self.portal_snap.x, self.portal_snap.y, self.portal_snap.z
            ));
        }

        // Check the portal is the correct distance from the origin.
        if self.portal_distance > 0.0
            && !is_multiple_of_eps(plane.d(), self.portal_distance, 1.0 / 1024.0)
        {
            *good = false;
            self.add_error(&format!(
                "Portal must be a multiple of {0:.6} away from the origin \
                 (it's {1:.6} away)",
                self.portal_distance,
                plane.d()
            ));
        }

        // Check the portals offset from the origin on it's plane is good.
        let mut offset = 0.0;
        if close(plane.normal().x.abs(), 1.0) {
            offset = portal_centre.z;
        }

        if close(plane.normal().z.abs(), 1.0) {
            offset = portal_centre.x;
        }

        if offset != 0.0 && self.portal_offset != 0.0 && !is_multiple_of(offset, self.portal_offset)
        {
            *good = false;
            self.add_error(&format!(
                "Centre of portal must be a multiple of {0:.6} on the \
                 portals plane (it's {1:.6} away)",
                self.portal_offset, offset
            ));
        }
    }

    /// `checkTriangleCount`: every `*indices` child contributes
    /// `nIndices_ / 3`.
    fn check_triangle_count(&mut self, prims: &mut BinSection) -> bool {
        let mut count: u32 = 0;

        // Iterate through all sections and find the indices sections.
        for section in prims.children() {
            if section.section_name().ends_with("indices")
                && let Some(header) = IndexHeader::read(section.as_binary().data())
            {
                // The reference wraps a negative count into the u32.
                count = count.wrapping_add((header.n_indices / 3) as u32);
            }
        }

        if count > self.max_triangles && self.max_triangles != 0 {
            self.add_error(&format!(
                "Too many triangles ({count}), must be less than {0}",
                self.max_triangles
            ));
            return false;
        }

        if count < self.min_triangles {
            self.add_error(&format!(
                "Too few triangles ({count}), must be at least {0}",
                self.min_triangles
            ));
            return false;
        }

        true
    }

    /// `addError`.
    fn add_error(&mut self, message: &str) {
        self.errors.push(message.to_string());
    }

    /// If `check` failed, get the errors that occurred, one per line.
    pub fn error_text(&self) -> String {
        self.errors.join("\n")
    }

    /// The type of visual we have determined from the file name.
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// The recommended type (static, static with nodes, normal) to export
    /// the visual as.
    pub fn export_as(&self) -> &str {
        &self.export_as
    }

    /// Sets the snap vertices flag.
    pub fn set_snap_vertices(&mut self, snap_vertices: bool) {
        self.snap_vertices = snap_vertices;
    }
}

// ----------------------------------------------------------------------
// Tests.
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use resources::datasection;

    /// A fixture root with its own `BwResource`.
    struct Fixture {
        dir: std::path::PathBuf,
        resource: BwResource,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("bw_resource_checker_{tag}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let resource = BwResource::init_paths(&[dir.to_str().unwrap()]);
            Self { dir, resource }
        }

        fn write(&self, name: &str, data: &(impl AsRef<[u8]> + ?Sized)) {
            let path = self.dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, data).unwrap();
        }

        /// Parse a written resource into its section.
        fn section(&self, name: &str) -> Option<DataSection> {
            let raw = std::fs::read(self.dir.join(name)).ok()?;
            datasection::parse(bwresource::get_filename(name), &raw).ok()
        }
    }

    /// A full rule set exercising every knob (the shipped
    /// `visual_rules.xml` shape).
    fn rules_xml() -> String {
        String::from(
            "<root>\n\
             <rule><path>structures/</path><filespec>*.static.visual</filespec>\n\
             <identifier>static</identifier><exportAs>static</exportAs>\n\
             <minSize>1 2 3</minSize><maxSize>100 200 300</maxSize>\n\
             <minTriangles>4</minTriangles><maxTriangles>40</maxTriangles>\n\
             <maxHierarchyDepth>2</maxHierarchyDepth><portals>true</portals>\n\
             <portalSnap>0.5 0.5 0.5</portalSnap><portalDistance>3</portalDistance>\n\
             <portalOffset>25</portalOffset><checkUnknownHardPoints>true</checkUnknownHardPoints>\n\
             <hardPoint>HP_Gun</hardPoint></rule>\n\
             <rule><path>structures/</path><filespec></filespec>\n\
             <identifier>static all</identifier><exportAs>static with nodes</exportAs>\n\
             <parent>static</parent></rule>\n\
             <rule><path></path><filespec></filespec>\n\
             <identifier>normal</identifier><exportAs>bad value</exportAs></rule>\n\
             </root>",
        )
    }

    /// A container with one `geometry0indices` section.
    fn primitives_bytes(n_indices: i32) -> Vec<u8> {
        let mut prims = BinSection::new("m.primitives", BinaryBlock::default());
        let mut blob = vec![0u8; 64];
        blob[..4].copy_from_slice(b"list");
        blob.extend_from_slice(&n_indices.to_le_bytes());
        blob.extend_from_slice(&0i32.to_le_bytes());
        prims
            .new_section("geometry0vertices")
            .set_binary(BinaryBlock::new(vec![0u8; 16]));
        prims
            .new_section("geometry0indices")
            .set_binary(BinaryBlock::new(blob));
        prims.save_bytes()
    }

    /// A good static visual: one node over the required hard point, a unit
    /// bounding box, a portal that sits on the snap grid, and a clean
    /// texture. Portal points are in plane space — the checker maps them
    /// through the portal basis (`uAxis`, `normal x uAxis`, `normal`) onto
    /// the +z face at d=3.
    fn good_visual() -> String {
        String::from(
            "<root>\
             <boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>root</identifier>\
             <node><identifier>HP_Gun</identifier></node></node>\
             <boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>1 0 0</uAxis>\
             <point>-0.5 1 0</point><point>0.5 1 0</point><point>0.5 2 0</point>\
             <point>-0.5 2 0</point></portal></boundary>\
             <renderSet><geometry><primitiveGroup>\
             <material><textureormfm>maps/stone.dds</textureormfm></material>\
             </primitiveGroup></geometry></renderSet>\
             </root>",
        )
    }

    fn write_fixtures(fixture: &Fixture, visual: &str, prim_bytes: &[u8]) {
        fixture.write("visual_rules.xml", &rules_xml());
        fixture.write("structures/houses/house.static.visual", visual);
        fixture.write("structures/houses/house.primitives", prim_bytes);
    }

    fn check_name(fixture: &Fixture, visual: &str, prim_bytes: &[u8]) -> (bool, VisualChecker) {
        write_fixtures(fixture, visual, prim_bytes);
        let mut checker =
            VisualChecker::new(&fixture.resource, "structures/houses/house.static.visual");
        let section = fixture.section("structures/houses/house.static.visual");
        let good = checker.check(
            &fixture.resource,
            section.as_ref(),
            "structures/houses/house.primitives",
        );
        (good, checker)
    }

    #[test]
    fn glob_matches_dos_wildcards_and_classes() {
        assert!(path_match_spec("house.visual", "*.visual"));
        assert!(path_match_spec("house.static.visual", "*.static.visual"));
        assert!(!path_match_spec("house.static.visual", "*.visual.visual"));
        assert!(path_match_spec("a1.model", "a?.model"));
        assert!(!path_match_spec("abc.model", "a?.model"));
        assert!(path_match_spec("x10.model", "x[0-9][0-9].model"));
        assert!(path_match_spec("x9.model", "x[!0-8].model"));
        assert!(!path_match_spec("x0.model", "x[!0-8].model"));
        assert!(path_match_spec("", "*"));
        assert!(!path_match_spec("a", ""));
        // A leading `]` in a class is a literal, not the terminator.
        assert!(path_match_spec("lit].model", "[]l]it].model"));
        assert!(path_match_spec("]it].model", "[]l]it].model"));
        assert!(!path_match_spec("kit].model", "[]l]it].model"));
    }

    #[test]
    fn missing_rules_file_is_recorded() {
        let fixture = Fixture::new("norules");
        let checker = VisualChecker::new(&fixture.resource, "somewhere/house.visual");
        assert_eq!(
            checker.error_text(),
            "VisualChecker::VisualChecker - Unable to find visual_rules.xml file."
        );
        assert_eq!(checker.type_name(), UNKNOWN_TYPE_NAME);
        assert_eq!(checker.export_as(), "normal");
    }

    #[test]
    fn collision_visuals_skip_everything() {
        // No rules file at all, yet no error: the constructor returned
        // before opening the rules.
        let fixture = Fixture::new("bsp");
        let mut checker = VisualChecker::new(&fixture.resource, "house_bsp.visual");
        assert_eq!(checker.error_text(), "");
        assert_eq!(checker.type_name(), UNKNOWN_TYPE_NAME);
        checker.set_snap_vertices(false);
    }

    #[test]
    fn a_good_visual_passes_with_every_rule_bound() {
        let fixture = Fixture::new("good");
        let prim_bytes = primitives_bytes(12);
        let (good, checker) = check_name(&fixture, &good_visual(), &prim_bytes);
        assert!(good, "errors: {}", checker.error_text());
        assert_eq!(checker.error_text(), "");
        // The `*.static.visual` rule wins over the same-length catch-all.
        assert_eq!(checker.type_name(), "static");
        assert_eq!(checker.export_as(), "static");
    }

    #[test]
    fn size_and_triangle_bounds_are_reported() {
        let fixture = Fixture::new("bounds");

        // A tiny visual with almost no triangles (min is 4, size 1x2x3).
        let tiny = String::from(
            "<root><boundingBox><min>0 0 0</min><max>1 2 3</max></boundingBox>\
             <node><identifier>root</identifier></node></root>",
        );
        let (good, checker) = check_name(&fixture, &tiny, &primitives_bytes(3));
        assert!(!good);
        assert_eq!(
            checker.error_text(),
            "Too few triangles (1), must be at least 4\n\
             Must have one or more portals\n\
             Missing hard point HP_Gun"
        );

        // A huge visual with too many triangles.
        let huge = String::from(
            "<root><boundingBox><min>-90 -90 -90</min><max>90 90 90</max></boundingBox>\
             <node><identifier>root</identifier></node></root>",
        );
        let (good, checker) = check_name(&fixture, &huge, &primitives_bytes(129));
        assert!(!good);
        let errors = checker.error_text();
        assert!(
            errors.contains("Too big, must be no bigger than 100.000000, 200.000000, 300.000000")
        );
        assert!(errors.contains("Too many triangles (43), must be less than 40"));
    }

    #[test]
    fn hierarchy_depth_is_bounded() {
        let fixture = Fixture::new("depth");
        // Three node levels over the limit; everything else about the
        // visual is fine, so this is the only error.
        let deep = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>root</identifier>\
             <node><identifier>mid</identifier>\
             <node><identifier>HP_Gun</identifier></node></node></node>",
        ) + "<boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>1 0 0</uAxis>\
             <point>-0.5 1 0</point><point>0.5 1 0</point><point>0.5 2 0</point>\
             <point>-0.5 2 0</point></portal></boundary></root>";
        let (good, checker) = check_name(&fixture, &deep, &primitives_bytes(12));
        assert!(!good);
        assert_eq!(
            checker.error_text(),
            "Node hierarchy too deep, depth must be 2 nodes or less"
        );

        // Two levels pass; three do not.
        let shallow = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>root</identifier>\
             <node><identifier>HP_Gun</identifier></node></node>\
             <boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>1 0 0</uAxis>\
             <point>-0.5 1 0</point><point>0.5 1 0</point><point>0.5 2 0</point>\
             <point>-0.5 2 0</point></portal></boundary></root>",
        );
        let (good, checker) = check_name(&fixture, &shallow, &primitives_bytes(12));
        assert!(good, "errors: {}", checker.error_text());
    }

    #[test]
    fn portal_battery_reports_each_failure() {
        let fixture = Fixture::new("portal");

        // Not enough points.
        let few = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>HP_Gun</identifier></node>\
             <boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>1 0 0</uAxis><point>0 0 0</point></portal>\
             </boundary></root>",
        );
        let (good, checker) = check_name(&fixture, &few, &primitives_bytes(12));
        assert!(!good);
        assert!(
            checker
                .error_text()
                .contains("Not enough points in a portal")
        );

        // A portal on the -z face (boundary normal points inward, so the
        // transform is (u, v, -3.25)), off the snap grid, at a bad
        // distance. Points are wound so the plane normal points into the
        // box, keeping the facing check quiet.
        let bad = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>HP_Gun</identifier></node>\
             <boundary><normal>0 0 1</normal><d>-3.25</d>\
             <portal><uAxis>1 0 0</uAxis>\
             <point>-0.3 2 0</point><point>0.3 2 0</point>\
             <point>0.3 1 0</point><point>-0.3 1 0</point></portal>\
             </boundary></root>",
        );
        let (good, checker) = check_name(&fixture, &bad, &primitives_bytes(12));
        assert!(!good);
        let errors = checker.error_text();
        assert!(
            errors.contains("Portals must reside on multiples of 0.500000, 0.500000, 0.500000")
        );
        assert!(errors.contains(
            "Portal must be a multiple of 3.000000 away from the origin (it's -3.250000 away)"
        ));
    }

    #[test]
    fn portal_facings_and_planarity() {
        let fixture = Fixture::new("facing");

        // The plane normal points out of the box: "facing outwards".
        // uAxis (0,1,0) maps plane point (u,v,0) to world (-v,u,3); the
        // winding below puts the plane normal at (0,0,1), away from the
        // box centre.
        let outward = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>HP_Gun</identifier></node>\
             <boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>0 1 0</uAxis>\
             <point>1 -0.5 0</point><point>2 0.5 0</point>\
             <point>2 -0.5 0</point><point>1 -0.5 0</point></portal>\
             </boundary></root>",
        );
        let (good, checker) = check_name(&fixture, &outward, &primitives_bytes(12));
        assert!(!good);
        assert!(checker.error_text().contains("Portal is facing outwards"));

        // Colinear points give a zero normal: "non planar".
        let colinear = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>HP_Gun</identifier></node>\
             <boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>0 1 0</uAxis>\
             <point>0 0 0</point><point>0 -1 0</point><point>0 -2 0</point></portal>\
             </boundary></root>",
        );
        let (good, checker) = check_name(&fixture, &colinear, &primitives_bytes(12));
        assert!(!good);
        assert!(checker.error_text().contains("Portal is non planar"));
    }

    #[test]
    fn portals_are_forbidden_on_normal_visuals() {
        // The catch-all rule inherits nothing: portals stay false, so the
        // good visual's portal is now an error.
        let fixture = Fixture::new("noportals");
        fixture.write(
            "visual_rules.xml",
            "<root><rule><path></path><filespec></filespec>\
             <identifier>normal</identifier></rule></root>",
        );
        fixture.write("structures/house.visual", &good_visual());
        fixture.write("structures/house.primitives", &primitives_bytes(12));
        let mut checker = VisualChecker::new(&fixture.resource, "structures/house.visual");
        let section = fixture.section("structures/house.visual");
        let good = checker.check(
            &fixture.resource,
            section.as_ref(),
            "structures/house.primitives",
        );
        assert!(!good);
        assert_eq!(checker.error_text(), "Must not have any portals");
    }

    #[test]
    fn duplicate_nodes_and_hard_points_are_judged() {
        let fixture = Fixture::new("nodes");

        let bad = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>root</identifier>\
             <node><identifier>root</identifier></node>\
             <node><identifier>HP_Left</identifier></node></node>\
             <boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>1 0 0</uAxis>\
             <point>-0.5 1 0</point><point>0.5 1 0</point><point>0.5 2 0</point>\
             <point>-0.5 2 0</point></portal></boundary></root>",
        );
        let (good, checker) = check_name(&fixture, &bad, &primitives_bytes(12));
        assert!(!good);
        assert_eq!(
            checker.error_text(),
            "Duplicate node name root\n\
             Missing hard point HP_Gun\n\
             Unknown hard point HP_Left"
        );

        // The flash exemption: HP_flash01 (10 chars, digits) and the
        // bare HP_flash are both legal without a rule.
        let flash = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>HP_flash01</identifier>\
             <node><identifier>HP_flash</identifier></node></node>\
             <boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>1 0 0</uAxis>\
             <point>-0.5 1 0</point><point>0.5 1 0</point><point>0.5 2 0</point>\
             <point>-0.5 2 0</point></portal></boundary></root>",
        );
        let (good, checker) = check_name(&fixture, &flash, &primitives_bytes(12));
        assert!(!good);
        assert_eq!(checker.error_text(), "Missing hard point HP_Gun");
    }

    #[test]
    fn texture_names_must_not_have_spaces() {
        let fixture = Fixture::new("texture");
        let spaced = String::from(
            "<root><boundingBox><min>-1 -2 -3</min><max>1 2 3</max></boundingBox>\
             <node><identifier>HP_Gun</identifier></node>\
             <boundary><normal>0 0 1</normal><d>3</d>\
             <portal><uAxis>1 0 0</uAxis>\
             <point>-0.5 1 0</point><point>0.5 1 0</point><point>0.5 2 0</point>\
             <point>-0.5 2 0</point></portal></boundary>\
             <renderSet><geometry><primitiveGroup>\
             <material><textureormfm>maps/big stone.dds</textureormfm></material>\
             </primitiveGroup></geometry></renderSet></root>",
        );
        let (good, checker) = check_name(&fixture, &spaced, &primitives_bytes(12));
        assert!(!good);
        assert!(checker.error_text().contains(
            "Illegal texture name \"maps/big stone.dds\", no spaces in name or path allowed"
        ));
    }

    #[test]
    fn invalid_export_as_resets_to_normal() {
        let fixture = Fixture::new("exportas");
        fixture.write("plain.visual", &good_visual());
        fixture.write(
            "visual_rules.xml",
            "<root><rule><path></path><filespec></filespec>\
             <identifier>normal</identifier><exportAs>weird</exportAs></rule></root>",
        );
        fixture.write("plain.primitives", &primitives_bytes(0));
        let mut checker = VisualChecker::new(&fixture.resource, "plain.visual");
        // The constructor flags the bad value and resets it; check() then
        // clears the error list like the reference does.
        assert_eq!(checker.error_text(), "Unknown value for exportAs [weird]\n");
        assert_eq!(checker.export_as(), "normal");
        let section = fixture.section("plain.visual");
        let good = checker.check(&fixture.resource, section.as_ref(), "plain.primitives");
        assert!(!good);
        assert_eq!(checker.error_text(), "Must not have any portals");
    }

    #[test]
    fn missing_visual_and_primitive_files_report() {
        let fixture = Fixture::new("missing");
        let mut checker = VisualChecker::new(&fixture.resource, "x.visual");
        assert!(!checker.check(&fixture.resource, None, "x.primitives"));
        assert_eq!(checker.error_text(), "Invalid visual, null DataSection");

        let section = datasection::parse("root", b"<root/>").ok();
        assert!(!checker.check(&fixture.resource, section.as_ref(), "missing.primitives"));
        assert_eq!(
            checker.error_text(),
            "Invalid primitive file, file not found"
        );
    }

    #[test]
    fn empty_indices_children_contribute_zero_triangles() {
        // A visual whose primitives container holds no *indices section.
        let fixture = Fixture::new("emptyindices");
        let mut prims = BinSection::new("m.primitives", BinaryBlock::default());
        prims
            .new_section("geometry0vertices")
            .set_binary(BinaryBlock::new(vec![0u8; 8]));
        fixture.write("visual_rules.xml", &rules_xml());
        fixture.write("structures/houses/house.static.visual", &good_visual());
        fixture.write("structures/houses/house.primitives", &prims.save_bytes());
        let mut checker =
            VisualChecker::new(&fixture.resource, "structures/houses/house.static.visual");
        let section = fixture.section("structures/houses/house.static.visual");
        let good = checker.check(
            &fixture.resource,
            section.as_ref(),
            "structures/houses/house.primitives",
        );
        assert!(!good);
        assert!(
            checker
                .error_text()
                .starts_with("Too few triangles (0), must be at least 4")
        );
    }
}
