window.SIDEBAR_ITEMS = {"constant":[["SERVER_STARTUP_RETRIES","Number of times to poll before failing"],["SERVER_STARTUP_SLEEP_MS","Number of milliseconds to sleep between attempts"]],"enum":[["DiscoKey","Configuration keys for Tide Disco settings"],["HealthStatus",""],["StatusCode","HTTP response status codes."],["UrlSegment",""]],"fn":[["app_api_path",""],["check_api","Check api.toml for schema compliance errors"],["compose_config_path","Compose the path to the application’s configuration file"],["compose_settings","Get the application configuration"],["configure_router","Add routes from api.toml to the routefinder instance in tide-disco"],["get_api_path","Get the path to `api.toml`"],["healthcheck","Return a JSON expression with status 200 indicating the server is up and running. The JSON expression is normally {“status”: “Available”} When the server is running but unable to process requests normally, a response with status 503 and payload {“status”: “unavailable”} should be added."],["init_logging",""],["load_api","Load the web API or panic"],["org_data_path",""],["wait_for_server","Wait for the server to respond to a connection request"]],"mod":[["api",""],["app",""],["error",""],["healthcheck",""],["method","Interfaces for methods of accessing to state."],["request",""],["route",""],["socket","An interface for asynchronous communication with clients, using WebSockets."]],"struct":[["DiscoArgs",""],["ServerState",""],["Url","A parsed URL record."]],"type":[["AppServerState",""],["AppState",""],["Html",""]]};