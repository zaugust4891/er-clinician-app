# ---------------------------------------------------------------------------
# Parameter group
# ---------------------------------------------------------------------------
# `rds.force_ssl = 1` makes the server refuse plaintext sessions. That is one half of
# the guarantee: it encrypts the connection, but it does not tell the client which
# server it reached. The other half lives in the application — `db.rs` pins
# `sslmode=verify-full` with `DB_SSL_ROOT_CERT` outside development, so the client
# verifies both the certificate chain and the hostname. Neither half is sufficient alone.
# https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/PostgreSQL.Concepts.General.SSL.html

resource "aws_db_parameter_group" "postgres" {
  name        = "${var.name}-${local.postgres_family}"
  family      = local.postgres_family
  description = "${var.name} PostgreSQL: TLS required, statement text never logged"

  parameter {
    name  = "rds.force_ssl"
    value = "1"
    # Safe for a static or a dynamic parameter. The instance below is created with this
    # group attached, so the setting is in force from first boot; a later change to it
    # is applied on the next reboot rather than mid-session.
    apply_method = "pending-reboot"
  }

  # These match the RDS defaults. They are stated explicitly because the database holds
  # transcripts and clinical text: turning statement logging on would copy that text
  # into the engine log, and it should take a deliberate, reviewed edit to do so.
  parameter {
    name         = "log_statement"
    value        = "none"
    apply_method = "immediate"
  }

  parameter {
    name         = "log_min_duration_statement"
    value        = "-1"
    apply_method = "immediate"
  }

  lifecycle {
    create_before_destroy = true
  }
}

# ---------------------------------------------------------------------------
# The database
# ---------------------------------------------------------------------------
# Single instance, private placement, encrypted storage and backups, deletion protected.
# It holds events, snapshots, audio-chunk transcripts, notes and usage attribution, so
# it is the largest concentration of clinical content in the system even though staging
# is synthetic. Backups and snapshots are separate copies of all of it.

resource "aws_db_instance" "this" {
  identifier = local.db_identifier

  engine = "postgres"
  # A bare major version lets RDS choose a currently supported minor at creation time
  # instead of pinning one this repository cannot verify. Check the class and storage
  # combination with `describe-orderable-db-instance-options` before the first apply.
  engine_version              = var.postgres_version
  auto_minor_version_upgrade  = true
  allow_major_version_upgrade = false
  parameter_group_name        = aws_db_parameter_group.postgres.name

  instance_class        = var.db_instance_class
  storage_type          = "gp3"
  allocated_storage     = var.db_allocated_storage
  max_allocated_storage = var.db_max_allocated_storage
  storage_encrypted     = true
  kms_key_id            = aws_kms_key.data.arn

  db_name  = var.db_name
  username = var.db_master_username
  # RDS creates and stores the master password in Secrets Manager under the application
  # data key. No password is passed through Terraform, so none is written to state.
  # S6 uses this credential once, from a one-shot task inside the VPC, to create the
  # schema-owner and runtime logins; no running service ever receives it.
  manage_master_user_password   = true
  master_user_secret_kms_key_id = aws_kms_key.data.arn

  # Private by construction: no public endpoint, isolated subnets with no route to the
  # internet gateway or NAT, and ingress only from the four approved task groups.
  publicly_accessible    = false
  db_subnet_group_name   = var.db_subnet_group_name
  vpc_security_group_ids = var.db_security_group_ids
  multi_az               = var.db_multi_az
  ca_cert_identifier     = var.db_ca_cert_identifier

  backup_retention_period  = var.db_backup_retention_days
  backup_window            = var.db_backup_window
  maintenance_window       = var.db_maintenance_window
  copy_tags_to_snapshot    = true
  delete_automated_backups = false

  # Two separate brakes on accidental loss: AWS refuses the delete call while deletion
  # protection is on, and Terraform must name a final snapshot when it is turned off.
  # Teardown is therefore a reviewed apply that changes both, not a stray `destroy`.
  deletion_protection       = var.db_deletion_protection
  skip_final_snapshot       = false
  final_snapshot_identifier = "${local.db_identifier}-final-${var.db_final_snapshot_suffix}"

  apply_immediately = var.db_apply_immediately

  # Enhanced monitoring, Performance Insights and engine log export are S9's telemetry
  # decisions: each one creates a new destination with its own retention, and exporting
  # engine logs before S9 sets retention would create an unmanaged copy.
}
