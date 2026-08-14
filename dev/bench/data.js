window.BENCHMARK_DATA = {
  "lastUpdate": 1786675874982,
  "repoUrl": "https://github.com/lucasacoutinho/phpantom_lsp",
  "entries": {
    "PHPantom Benchmarks": [
      {
        "commit": {
          "author": {
            "email": "anders@jenbo.dk",
            "name": "Anders Jenbo",
            "username": "AJenbo"
          },
          "committer": {
            "email": "anders@jenbo.dk",
            "name": "Anders Jenbo",
            "username": "AJenbo"
          },
          "distinct": true,
          "id": "72f4a10927cc9a967bd48d765c02f98c492f86c9",
          "message": "Fix a few issues",
          "timestamp": "2026-08-14T04:24:12+02:00",
          "tree_id": "5315a901356769fe3b44ee1c43309f40bf6b7223",
          "url": "https://github.com/lucasacoutinho/phpantom_lsp/commit/72f4a10927cc9a967bd48d765c02f98c492f86c9"
        },
        "date": 1786675874218,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold_start_completion",
            "value": 4.115,
            "range": "± 0.058",
            "unit": "ms"
          },
          {
            "name": "completion_simple_class",
            "value": 0.045,
            "range": "± 0.005",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_5",
            "value": 0.102,
            "range": "± 0.008",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_10",
            "value": 0.165,
            "range": "± 0.008",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_20",
            "value": 0.231,
            "range": "± 0.016",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/100_classes",
            "value": 0.301,
            "range": "± 0.017",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/500_classes",
            "value": 1.2,
            "range": "± 0.025",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/1000_classes",
            "value": 2.316,
            "range": "± 0.079",
            "unit": "ms"
          },
          {
            "name": "completion_generics_and_mixins",
            "value": 0.145,
            "range": "± 0.004",
            "unit": "ms"
          },
          {
            "name": "completion_with_narrowing",
            "value": 0.061,
            "range": "± 0.005",
            "unit": "ms"
          },
          {
            "name": "completion_5_method_chain",
            "value": 0.057,
            "range": "± 0.005",
            "unit": "ms"
          },
          {
            "name": "completion_cross_file_type_hint",
            "value": 0.07,
            "range": "± 0.009",
            "unit": "ms"
          },
          {
            "name": "completion_carbon_class",
            "value": 5.441,
            "range": "± 0.019",
            "unit": "ms"
          },
          {
            "name": "completion_yii_deep_hierarchy",
            "value": 0.142,
            "range": "± 0.006",
            "unit": "ms"
          },
          {
            "name": "completion_large_file",
            "value": 0.369,
            "range": "± 0.023",
            "unit": "ms"
          },
          {
            "name": "completion_short_file",
            "value": 0.081,
            "range": "± 0.009",
            "unit": "ms"
          },
          {
            "name": "variable_completion/short",
            "value": 0.049,
            "range": "± 0.004",
            "unit": "ms"
          },
          {
            "name": "variable_completion/long",
            "value": 0.123,
            "range": "± 0.006",
            "unit": "ms"
          },
          {
            "name": "hover_method_call",
            "value": 0.123,
            "range": "± 0.009",
            "unit": "ms"
          },
          {
            "name": "goto_definition_method",
            "value": 0.088,
            "range": "± 0.009",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/100_lines",
            "value": 0.22,
            "range": "± 0.008",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/500_lines",
            "value": 1.126,
            "range": "± 0.009",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/2000_lines",
            "value": 5.733,
            "range": "± 0.112",
            "unit": "ms"
          },
          {
            "name": "reparse_500_line_file",
            "value": 1.131,
            "range": "± 0.015",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_new_generic_objects",
            "value": 0.036,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_new_objects",
            "value": 0.035,
            "range": "± 0.000",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_missing_methods",
            "value": 55.07,
            "range": "± 0.296",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/method_chain",
            "value": 2.664,
            "range": "± 0.026",
            "unit": "ms"
          }
        ]
      }
    ]
  }
}