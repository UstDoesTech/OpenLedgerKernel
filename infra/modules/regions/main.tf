# The regions each geo may use. One source of truth: ../../geos.json, which the region guard
# (scripts/check_uk_regions.py) also reads. Every region listed for a geo is allowed in it.
# Other modules take their regions from here and never write a region by hand.
locals {
  config  = jsondecode(file("${path.module}/../../geos.json"))
  geo_ok  = contains(local.config.enabled, var.geo)
  regions = local.config.geos[var.geo]
}
