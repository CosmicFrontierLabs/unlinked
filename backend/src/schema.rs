// @generated automatically by Diesel CLI.

diesel::table! {
    audit_log (id) {
        id -> Uuid,
        actor_id -> Nullable<Uuid>,
        org_id -> Nullable<Uuid>,
        project_id -> Nullable<Uuid>,
        action -> Text,
        detail -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    file_versions (id) {
        id -> Uuid,
        file_id -> Uuid,
        version -> Int4,
        content -> Bytea,
        sha256 -> Text,
        size_bytes -> Int8,
        author_id -> Nullable<Uuid>,
        message -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    files (id) {
        id -> Uuid,
        project_id -> Uuid,
        path -> Text,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        deleted_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    org_members (org_id, user_id) {
        org_id -> Uuid,
        user_id -> Uuid,
        role -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    organizations (id) {
        id -> Uuid,
        name -> Text,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    project_members (project_id, user_id) {
        project_id -> Uuid,
        user_id -> Uuid,
        role -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    projects (id) {
        id -> Uuid,
        org_id -> Uuid,
        name -> Text,
        description -> Text,
        default_role -> Text,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    sessions (id) {
        id -> Uuid,
        user_id -> Uuid,
        token_hash -> Text,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    simulation_runs (id) {
        id -> Uuid,
        file_version_id -> Uuid,
        requested_by -> Nullable<Uuid>,
        status -> Text,
        request -> Jsonb,
        trace -> Nullable<Jsonb>,
        error -> Nullable<Text>,
        created_at -> Timestamptz,
        finished_at -> Nullable<Timestamptz>,
        signals -> Jsonb,
    }
}

diesel::table! {
    user_identities (id) {
        id -> Uuid,
        user_id -> Uuid,
        provider -> Text,
        subject -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    users (id) {
        id -> Uuid,
        email -> Text,
        name -> Nullable<Text>,
        avatar_url -> Nullable<Text>,
        created_at -> Timestamptz,
        last_login_at -> Nullable<Timestamptz>,
    }
}

diesel::joinable!(audit_log -> organizations (org_id));
diesel::joinable!(audit_log -> projects (project_id));
diesel::joinable!(audit_log -> users (actor_id));
diesel::joinable!(file_versions -> files (file_id));
diesel::joinable!(file_versions -> users (author_id));
diesel::joinable!(files -> projects (project_id));
diesel::joinable!(files -> users (created_by));
diesel::joinable!(org_members -> organizations (org_id));
diesel::joinable!(org_members -> users (user_id));
diesel::joinable!(organizations -> users (created_by));
diesel::joinable!(project_members -> projects (project_id));
diesel::joinable!(project_members -> users (user_id));
diesel::joinable!(projects -> organizations (org_id));
diesel::joinable!(projects -> users (created_by));
diesel::joinable!(sessions -> users (user_id));
diesel::joinable!(simulation_runs -> file_versions (file_version_id));
diesel::joinable!(simulation_runs -> users (requested_by));
diesel::joinable!(user_identities -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(
    audit_log,
    file_versions,
    files,
    org_members,
    organizations,
    project_members,
    projects,
    sessions,
    simulation_runs,
    user_identities,
    users,
);
