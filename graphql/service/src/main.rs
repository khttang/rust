use async_graphql::{
    http::{playground_source, GraphQLPlaygroundConfig},
    *,
};
use async_graphql_axum::{GraphQL};
//Khiem use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    response::{Html, IntoResponse},
    routing::get,
    // Khiem Extension,
    Router,
};

// 1. Define your data model
#[derive(SimpleObject)]
struct User {
    id: ID,
    name: String,
    age: i32,
}

// 2. Define your GraphQL Query
pub struct Query;

#[Object]
impl Query {
    /// Returns a simple string
    async fn hello(&self) -> &'static str {
        "Hello from Axum and async-graphql!"
    }

    /// Returns a specific User
    async fn user(&self) -> User {
        User {
            id: ID("123".to_string()),
            name: "Alice Doe".to_string(),
            age: 30,
        }
    }
}

// 3. Define your GraphQL Mutation
pub struct Mutation;

#[Object]
impl Mutation {
    /// Increments the given user's age by 1
    async fn update_age(&self, name: String, current_age: i32) -> User {
        User {
            id: ID("456".to_string()),
            name,
            age: current_age + 1,
        }
    }
}

// 4. Define your GraphQL Schema
//KHIEM type AppSchema = Schema<Query, Mutation, EmptySubscription>;

// 5. Axum Handlers
// Khiem: This handle is only needed when async_graphql_axum::GraphQL is not imported or used
//async fn graphql_handler(schema: Extension<AppSchema>, req: GraphQLRequest) -> GraphQLResponse {
//    schema.execute(req.into_inner()).await.into()
//}

async fn graphql_playground() -> impl IntoResponse {
    Html(playground_source(
        GraphQLPlaygroundConfig::new("/graphql").subscription_endpoint("/ws"),
    ))
}

// 6. Main function to set up Axum Router and Server
#[tokio::main]
async fn main() {
    let schema = Schema::build(Query, Mutation, EmptySubscription).finish();

    let app = Router::new()
        .route(
            "/graphql",
            get(graphql_playground).post_service(GraphQL::new(schema)),
        );
        // Khiem Alternatively, use the explicit handler approach:
        //.route("/graphql", get(graphql_playground).post(graphql_handler))
        //.layer(Extension(schema));

    println!("GraphiQL IDE: http://localhost:8000/graphql");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}