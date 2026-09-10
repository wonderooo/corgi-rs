-- Extract corgi-rs lookup assets from a full NHTSA vPIC database.
--
-- Input:  the vPIC schema as published at https://vpic.nhtsa.dot.gov/Downloads
--         (the complete database -- not a slimmed-down redistribution).
-- Output: tab-separated files in psql's working directory, each sorted by its lookup key so
--         that build.rs can stream them straight into an fst map.
--
-- The queries mirror vpic.spvindecode_core: same schema/year filter, same
-- element exclusions, same attribute resolution via felementattributevalue.

\set ON_ERROR_STOP on
\timing off

-- Vehicle types we keep. 2 = Passenger Car, 3 = Truck (covers every pickup),
-- 7 = MPV (covers every crossover/SUV), 10 = Incomplete Vehicle (cutaway vans
-- and chassis that still show up at salvage auctions), and 5 = Bus, which is
-- how NHTSA files 12- and 15-seat passenger vans: a Ford Transit Wagon, a
-- Chevrolet Express and a Mercedes Sprinter are all "buses" to vPIC, and all
-- three are ordinary auction lots.
--
-- Motorcycles, trailers, low-speed and off-road vehicles are dropped. They are
-- ~85% of the WMI table and none of them are cars.
CREATE TEMP TABLE keep_wmi AS
SELECT w.id, upper(w.wmi) AS wmi, w.vehicletypeid, w.trucktypeid,
       w.manufacturerid, w.countryid
FROM vpic.wmi w
WHERE w.vehicletypeid IN (2, 3, 5, 7, 10)
  AND (w.publicavailabilitydate IS NULL OR w.publicavailabilitydate <= now());

CREATE INDEX ON keep_wmi (id);

-- Schemas reachable from a kept WMI. `tobeqced` schemas are drafts that
-- spvindecode_core hides from public decodes.
CREATE TEMP TABLE keep_schema AS
SELECT DISTINCT wvs.vinschemaid AS sid
FROM vpic.wmi_vinschema wvs
JOIN keep_wmi w ON w.id = wvs.wmiid
JOIN vpic.vinschema vs ON vs.id = wvs.vinschemaid
WHERE coalesce(vs.tobeqced, false) = false;

CREATE INDEX ON keep_schema (sid);

-- Elements that spvindecode_core is willing to emit from a VIN pattern.
-- 26/27/29/39 (Make, Manufacturer, ModelYear, VehicleType) are derived
-- elsewhere; private elements are NHTSA-internal (NCSA mappings).
CREATE TEMP TABLE keep_element AS
SELECT e.id, e.code, e.datatype
FROM vpic.element e
WHERE e.decode IS NOT NULL
  AND coalesce(e.isprivate, false) = false
  AND e.id NOT IN (26, 27, 29, 39);

CREATE INDEX ON keep_element (id);

-- Strip separators that would corrupt the TSV. vPIC values are free text in a
-- handful of columns (Note, OtherEngineInfo, ...).
CREATE OR REPLACE FUNCTION pg_temp.tsv(v text) RETURNS text AS $$
  SELECT btrim(regexp_replace(coalesce(v, ''), '[\t\r\n]+', ' ', 'g'));
$$ LANGUAGE sql IMMUTABLE;

-- Resolve a lookup AttributeId to its display value. Wraps NHTSA's own
-- resolver because a few private elements point at tables the public download
-- omits; for those the raw id is the best we have, and key matching compares
-- raw ids anyway.
CREATE OR REPLACE FUNCTION pg_temp.attrval(eid int, aid text) RETURNS text AS $fn$
BEGIN
  RETURN vpic.felementattributevalue(eid, aid);
EXCEPTION WHEN others THEN
  RETURN aid;
END;
$fn$ LANGUAGE plpgsql;

--------------------------------------------------------------------------------
-- wmi.tsv: one row per WMI.
--------------------------------------------------------------------------------
\copy (SELECT w.wmi, coalesce(w.vehicletypeid,0), coalesce(w.trucktypeid,0), coalesce(sole.makeid,0), pg_temp.tsv(sole.makename), pg_temp.tsv(mf.name), pg_temp.tsv(c.name) FROM keep_wmi w LEFT JOIN vpic.manufacturer mf ON mf.id = w.manufacturerid LEFT JOIN vpic.country c ON c.id = w.countryid LEFT JOIN LATERAL (SELECT min(mk.id) AS makeid, min(mk.name) AS makename FROM vpic.wmi_make wm JOIN vpic.make mk ON mk.id = wm.makeid WHERE wm.wmiid = w.id HAVING count(*) = 1) sole ON true ORDER BY w.wmi COLLATE "C") TO 'wmi.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')

--------------------------------------------------------------------------------
-- wmi_schema.tsv: which schemas a WMI may use, and for which model years.
-- This year range is the single most important filter in the whole decoder.
--------------------------------------------------------------------------------
\copy (SELECT w.wmi, wvs.vinschemaid, wvs.yearfrom, coalesce(wvs.yearto, 2999) FROM vpic.wmi_vinschema wvs JOIN keep_wmi w ON w.id = wvs.wmiid JOIN keep_schema ks ON ks.sid = wvs.vinschemaid ORDER BY w.wmi COLLATE "C", wvs.vinschemaid) TO 'wmi_schema.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')

--------------------------------------------------------------------------------
-- pattern.tsv: the VIN patterns themselves, with lookup ids already resolved
-- to their display value.
--------------------------------------------------------------------------------
\copy (SELECT p.vinschemaid, upper(p.keys), p.elementid, pg_temp.tsv(p.attributeid), pg_temp.tsv(vpic.felementattributevalue(p.elementid, p.attributeid)), coalesce(extract(epoch FROM coalesce(p.updatedon, p.createdon))::bigint, 0), p.id FROM vpic.pattern p JOIN keep_schema ks ON ks.sid = p.vinschemaid JOIN keep_element e ON e.id = p.elementid ORDER BY p.vinschemaid::text COLLATE "C", p.id) TO 'pattern.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')

--------------------------------------------------------------------------------
-- model.tsv: model id -> make. spvindecode_core derives Make from the decoded
-- Model (Make_Model is 1:1), which is what keeps Ram/Fiat/Jeep apart on the
-- WMIs those makes share.
--------------------------------------------------------------------------------
\copy (SELECT mm.modelid, pg_temp.tsv(mo.name), mm.makeid, pg_temp.tsv(mk.name) FROM vpic.make_model mm JOIN vpic.make mk ON mk.id = mm.makeid JOIN vpic.model mo ON mo.id = mm.modelid ORDER BY mm.modelid::text COLLATE "C") TO 'model.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')

--------------------------------------------------------------------------------
-- vspec.tsv: the VehicleSpecSchema tables, keyed by the (make, vehicle type,
-- model, year) tuple the VIN patterns resolve to. year 0 means "any year".
-- These supply drive type, transmission, seats, and the driver-assist elements
-- that no VIN pattern carries.
--------------------------------------------------------------------------------
\copy (WITH sch AS (SELECT s.id, s.makeid, s.vehicletypeid, m.modelid, coalesce(y.year, 0) AS year FROM vpic.vehiclespecschema s JOIN vpic.vehiclespecschema_model m ON m.vehiclespecschemaid = s.id LEFT JOIN vpic.vehiclespecschema_year y ON y.vehiclespecschemaid = s.id WHERE s.vehicletypeid IN (2,3,5,7,10) AND coalesce(s.tobeqced, false) = false) SELECT sch.makeid || '|' || sch.vehicletypeid || '|' || sch.modelid || '|' || sch.year AS k, sp.id, CASE WHEN p.iskey THEN 1 ELSE 0 END, p.elementid, pg_temp.tsv(p.attributeid), pg_temp.tsv(pg_temp.attrval(p.elementid, p.attributeid)), coalesce(extract(epoch FROM coalesce(p.updatedon, p.createdon))::bigint, 0) FROM sch JOIN vpic.vspecschemapattern sp ON sp.schemaid = sch.id JOIN vpic.vehiclespecpattern p ON p.vspecschemapatternid = sp.id ORDER BY (sch.makeid || '|' || sch.vehicletypeid || '|' || sch.modelid || '|' || sch.year) COLLATE "C", sp.id, p.iskey DESC, p.elementid) TO 'vspec.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')

--------------------------------------------------------------------------------
-- engine_model.tsv: extra elements implied by a decoded engine model name.
--------------------------------------------------------------------------------
\copy (SELECT pg_temp.tsv(em.name), p.elementid, pg_temp.tsv(p.attributeid), pg_temp.tsv(vpic.felementattributevalue(p.elementid, p.attributeid)) FROM vpic.enginemodel em JOIN vpic.enginemodelpattern p ON p.enginemodelid = em.id JOIN keep_element e ON e.id = p.elementid WHERE btrim(em.name) <> '' ORDER BY pg_temp.tsv(em.name) COLLATE "C", p.elementid) TO 'engine_model.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')

--------------------------------------------------------------------------------
-- default_value.tsv: per-vehicle-type fallbacks, applied last.
--------------------------------------------------------------------------------
\copy (SELECT dv.vehicletypeid, dv.elementid, pg_temp.tsv(dv.defaultvalue), pg_temp.tsv(CASE WHEN e.datatype = 'lookup' AND dv.defaultvalue = '0' THEN 'Not Applicable' ELSE vpic.felementattributevalue(dv.elementid, dv.defaultvalue) END) FROM vpic.defaultvalue dv JOIN vpic.element e ON e.id = dv.elementid WHERE dv.defaultvalue IS NOT NULL AND dv.vehicletypeid IN (2,3,5,7,10) ORDER BY dv.vehicletypeid::text COLLATE "C", dv.elementid) TO 'default_value.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')

--------------------------------------------------------------------------------
-- element.tsv: element metadata, so the decoder can name and type its output.
--------------------------------------------------------------------------------
\copy (SELECT e.id, pg_temp.tsv(e.code), pg_temp.tsv(e.name), pg_temp.tsv(e.datatype), pg_temp.tsv(e.groupname) FROM vpic.element e WHERE e.code IS NOT NULL ORDER BY e.id) TO 'element.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')

--------------------------------------------------------------------------------
-- vehicle_type.tsv / manufacturer lookups used for the WMI-derived elements.
--------------------------------------------------------------------------------
\copy (SELECT t.id, pg_temp.tsv(t.name) FROM vpic.vehicletype t ORDER BY t.id) TO 'vehicle_type.tsv' WITH (FORMAT text, DELIMITER E'\t', NULL '')
