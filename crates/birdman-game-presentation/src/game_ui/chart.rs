use std::sync::OnceLock;

use birdman_game_core::{
    FlightEnergy, FlightRecord, FlightRecordAcceleration, FlightRecordPlaybackSample,
    FlightRecordQueryError, FlightRecordSample, FlightState, FlightTelemetry, NedPoint, NedVector,
    TailControlReport, TailPilotPositionCommand, WindError, flight_energy,
};

use super::{environment, types::*};

mod projection;
use projection::{
    ChartData, ChartDataCursor, ChartProjection, ChartReferenceLine, ChartWindVector,
};

const BASE_TICK_FONT_PX: f64 = 14.0;
const BASE_AXIS_FONT_PX: f64 = 16.0;

impl GameUiChart {
    /// Resolves a raw viewport pointer to the retained time axis; initial presses must hit the plot.
    pub fn pointer_time(&self, position: [f64; 2], rect: [f64; 4], initial: bool) -> Option<f64> {
        self.seek_target?;
        if !position.iter().chain(&rect).all(|value| value.is_finite())
            || rect[2] <= 0.0
            || rect[3] <= 0.0
        {
            return None;
        }
        let extent = self.presentation.viewport;
        let [left, top, right, bottom] = self.presentation.plot;
        let scale = (rect[2] / extent.width).min(rect[3] / extent.height);
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let plot_x = (position[0] - rect[0] - (rect[2] - extent.width * scale) / 2.0) / scale;
        let plot_y = (position[1] - rect[1] - (rect[3] - extent.height * scale) / 2.0) / scale;
        if !plot_x.is_finite() || !plot_y.is_finite() {
            return None;
        }
        if right <= left
            || (initial && (plot_x < left || plot_x > right || plot_y < top || plot_y > bottom))
        {
            return None;
        }
        Some(
            self.x_minimum
                + ((plot_x - left) / (right - left)).clamp(0.0, 1.0)
                    * (self.x_maximum - self.x_minimum),
        )
    }
}

pub(super) fn record_chart(
    record: &FlightRecord,
    kind: GameUiAnalysisChart,
    time: f64,
    extent: Option<GameUiChartExtent>,
    root_font_size: f64,
    aircraft: Option<GameUiRecordAircraft>,
) -> Result<GameUiChart, GameUiError> {
    let samples = record.samples();
    let first = samples
        .first()
        .ok_or(GameUiError::Record(FlightRecordQueryError::EmptyRecord))?;
    let cursor = record
        .sample_at_seconds(time)
        .map_err(GameUiError::Record)?;
    let duration = record.duration_seconds().map_err(GameUiError::Record)?;
    let mut chart = ChartData {
        presentation: initial_presentation(extent, root_font_size)?,
        x_axis_label: "飛行時間 (s)".to_owned(),
        y_axis_label: String::new(),
        x_minimum: 0.0,
        x_maximum: if duration > 0.0 { duration } else { 0.01 },
        y_minimum: 0.0,
        y_maximum: 1.0,
        equal_axis_scale: false,
        series: Vec::new(),
        vectors: Vec::new(),
        markers: Vec::new(),
        time_markers: Vec::new(),
        reference_lines: Vec::new(),
        cursor_x: Some(time),
        cursor_points: Vec::new(),
    };
    match kind {
        GameUiAnalysisChart::Map => map_chart(&mut chart, record)?,
        GameUiAnalysisChart::Altitude => {
            chart.y_axis_label = "高度 (m)".to_owned();
            let range = padded_range(
                samples
                    .iter()
                    .map(|sample| sample.telemetry.altitude_m)
                    .chain([0.0, first.telemetry.altitude_m]),
            );
            chart.y_minimum = range[0];
            chart.y_maximum = range[1];
            chart.series.push(series(
                "高度",
                "#70d6c8",
                samples
                    .iter()
                    .map(|sample| point(sample_time(record, sample), sample.telemetry.altitude_m))
                    .collect(),
            ));
            chart.reference_lines = vec![
                ChartReferenceLine {
                    value: 0.0,
                    label: "静水面".to_owned(),
                    color: "#85c7e8".to_owned(),
                },
                ChartReferenceLine {
                    value: first.telemetry.altitude_m,
                    label: format!("発進高度 {:.1} m", first.telemetry.altitude_m),
                    color: "#f2b35e".to_owned(),
                },
            ];
        }
        GameUiAnalysisChart::Speed => {
            chart.y_axis_label = "速度 (m/s)".to_owned();
            let range = padded_range(samples.iter().flat_map(|sample| {
                [
                    sample.telemetry.airspeed_mps,
                    sample.telemetry.groundspeed_mps,
                ]
            }));
            chart.y_minimum = range[0];
            chart.y_maximum = range[1];
            for (label, color, air_relative) in [
                ("対気速度", "#70d6c8", true),
                ("対地速度", "#f2b35e", false),
            ] {
                let speed = |sample: &FlightRecordSample| {
                    if air_relative {
                        sample.telemetry.airspeed_mps
                    } else {
                        sample.telemetry.groundspeed_mps
                    }
                };
                chart.series.push(series(
                    label,
                    color,
                    samples
                        .iter()
                        .map(|sample| point(sample_time(record, sample), speed(sample)))
                        .collect(),
                ));
            }
        }
        _ => analysis_series_chart(&mut chart, record, kind, aircraft),
    }
    let cursor = data_cursor(
        "game-analysis-plot",
        kind,
        [
            chart.x_minimum,
            chart.x_maximum,
            chart.y_minimum,
            chart.y_maximum,
        ],
        &chart.series,
        cursor,
        time,
        record,
        aircraft,
    );
    chart.cursor_x = cursor.cursor_x;
    chart.cursor_points = cursor.cursor_points;
    if let Some(selected) = cursor.selected_wind_vector {
        chart.vectors.insert(0, selected);
    }
    complete_presentation(&mut chart);
    Ok(chart.into_projected())
}

fn initial_presentation(
    extent: Option<GameUiChartExtent>,
    root_font_size: f64,
) -> Result<GameUiChartPresentation, GameUiError> {
    let scale = root_font_size / BASE_AXIS_FONT_PX;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(GameUiError::InvalidViewport);
    }
    let measurement_minimum = GameUiChartExtent {
        width: 140.0 * scale,
        height: 100.0 * scale,
    };
    let viewport = extent.unwrap_or(GameUiChartExtent {
        width: 1000.0 * scale,
        height: 520.0 * scale,
    });
    if !viewport.width.is_finite()
        || !viewport.height.is_finite()
        || viewport.width < measurement_minimum.width
        || viewport.height < measurement_minimum.height
    {
        return Err(GameUiError::InvalidViewport);
    }
    let axis = chart_text(
        String::new(),
        point(0.0, 0.0),
        root_font_size,
        400,
        GameUiChartTextAnchor::Middle,
        "#d5e0dc",
    );
    Ok(GameUiChartPresentation {
        viewport,
        measurement_minimum,
        scroll_overflow: true,
        plot: [
            64.0 * scale,
            26.0 * scale,
            viewport.width - 20.0 * scale,
            viewport.height - 48.0 * scale,
        ],
        tick_font_size: BASE_TICK_FONT_PX * scale,
        axis_font_size: root_font_size,
        x_ticks: Vec::new(),
        y_ticks: Vec::new(),
        grid_lines: Vec::new(),
        axes_color: "#9db8b8".to_owned(),
        grid_color: "#456168".to_owned(),
        grid_opacity: 1.0,
        x_axis: axis.clone(),
        y_axis: axis,
        marker_labels: Vec::new(),
        time_marker_labels: Vec::new(),
        reference_labels: Vec::new(),
        legend: Vec::new(),
        map_guides: None,
        viewport_minimum_rem: [14.0, 10.0],
        target_height_px: viewport.height,
        legend_font_rem: 0.85,
        legend_gap_rem: 0.75,
        legend_swatch_rem: [0.75, 0.75],
        legend_swatch_gap_rem: 0.35,
        legend_gradient_rem: [2.0, 0.35],
        legend_gradient_radius_rem: 0.2,
    })
}

fn complete_presentation(chart: &mut ChartData) {
    let presentation = &mut chart.presentation;
    let scale = presentation.axis_font_size / BASE_AXIS_FONT_PX;
    if chart.equal_axis_scale {
        let [left, top, right, bottom] = presentation.plot;
        let top = (top + 40.0 * scale).min(bottom - scale);
        let side = (right - left).min(bottom - top);
        let centre_x = (left + right) / 2.0;
        let centre_y = (top + bottom) / 2.0;
        presentation.plot = [
            centre_x - side / 2.0,
            centre_y - side / 2.0,
            centre_x + side / 2.0,
            centre_y + side / 2.0,
        ];
    }
    let [left, top, right, bottom] = presentation.plot;
    let width = right - left;
    let height = bottom - top;
    let projection = ChartProjection::new(
        [
            chart.x_minimum,
            chart.x_maximum,
            chart.y_minimum,
            chart.y_maximum,
        ],
        presentation.plot,
        scale,
    );
    let project = |position| projection.point(position);
    let tick_count_x = (width / (80.0 * scale)).floor().clamp(1.0, 4.0) as u32;
    let tick_count_y = (height / (32.0 * scale)).floor().clamp(1.0, 4.0) as u32;
    presentation.x_ticks = (0..=tick_count_x)
        .map(|index| {
            let ratio = f64::from(index) / f64::from(tick_count_x);
            let value = chart.x_minimum + ratio * (chart.x_maximum - chart.x_minimum);
            GameUiChartTick {
                value,
                ratio,
                text: chart_text(
                    format_chart_tick(
                        value,
                        (chart.x_maximum - chart.x_minimum) / f64::from(tick_count_x),
                    ),
                    point(left + ratio * width, bottom + 20.0 * scale),
                    presentation.tick_font_size,
                    400,
                    GameUiChartTextAnchor::Middle,
                    "#d5e0dc",
                ),
            }
        })
        .collect();
    presentation.y_ticks = (0..=tick_count_y)
        .map(|index| {
            let ratio = f64::from(index) / f64::from(tick_count_y);
            let value = chart.y_minimum + ratio * (chart.y_maximum - chart.y_minimum);
            GameUiChartTick {
                value,
                ratio,
                text: chart_text(
                    format_chart_tick(
                        value,
                        (chart.y_maximum - chart.y_minimum) / f64::from(tick_count_y),
                    ),
                    point(left - 10.0 * scale, bottom - ratio * height + 5.0 * scale),
                    presentation.tick_font_size,
                    400,
                    GameUiChartTextAnchor::End,
                    "#d5e0dc",
                ),
            }
        })
        .collect();
    presentation.grid_lines = (1..=3)
        .map(|index| {
            let y = top + height * f64::from(index) / 4.0;
            [point(left, y), point(right, y)]
        })
        .collect();
    presentation.x_axis = chart_text(
        chart.x_axis_label.clone(),
        point(
            (left + right) / 2.0,
            presentation.viewport.height - 8.0 * scale,
        ),
        presentation.axis_font_size,
        400,
        GameUiChartTextAnchor::Middle,
        "#d5e0dc",
    );
    presentation.y_axis = chart_text(
        chart.y_axis_label.clone(),
        point(
            (left - 48.0 * scale).max(presentation.axis_font_size),
            (top + bottom) / 2.0,
        ),
        presentation.axis_font_size,
        400,
        GameUiChartTextAnchor::Middle,
        "#d5e0dc",
    );
    presentation.y_axis.rotation_degrees = -90.0;
    let marker_text = |marker: &GameUiChartMarker, offset: [f64; 2], weight| {
        let position = project(marker.point);
        let estimated_width =
            marker.label.encode_utf16().count() as f64 * presentation.tick_font_size * 0.65;
        let position = point(
            (position.x + offset[0] * scale)
                .min(presentation.viewport.width - estimated_width - 4.0 * scale)
                .max(4.0 * scale),
            (position.y + offset[1] * scale)
                .min(presentation.viewport.height - 4.0 * scale)
                .max(presentation.axis_font_size),
        );
        chart_text(
            marker.label.clone(),
            position,
            presentation.tick_font_size,
            weight,
            GameUiChartTextAnchor::Start,
            "#f3f4e8",
        )
    };
    presentation.marker_labels = chart
        .markers
        .iter()
        .map(|marker| marker_text(marker, [10.0, -8.0], 600))
        .collect();
    presentation.time_marker_labels = chart
        .time_markers
        .iter()
        .map(|marker| marker_text(marker, [7.0, 18.0], 500))
        .collect();
    let mut previous_labels = Vec::new();
    for label in &mut presentation.time_marker_labels {
        if previous_labels.iter().any(|position: &GameUiChartPoint| {
            (position.x - label.position.x).abs() < 64.0 * scale
                && (position.y - label.position.y).abs() < 22.0 * scale
        }) {
            label.label.clear();
        } else {
            previous_labels.push(label.position);
        }
    }
    presentation.reference_labels = chart
        .reference_lines
        .iter()
        .map(|line| {
            chart_text(
                line.label.clone(),
                point(
                    left + 8.0 * scale,
                    project(point(chart.x_minimum, line.value)).y - 7.0 * scale,
                ),
                presentation.tick_font_size,
                500,
                GameUiChartTextAnchor::Start,
                "#f3f4e8",
            )
        })
        .collect();
    presentation.legend = chart
        .series
        .iter()
        .map(|series| GameUiChartLegend {
            source: GameUiReadoutSource::Static,
            label: series.label.clone(),
            color: series.color.clone(),
            segment_gradient: series
                .segment_colors
                .first()
                .zip(series.segment_colors.last())
                .map(|(first, last)| [first.clone(), last.clone()]),
        })
        .chain(chart.markers.iter().map(|marker| GameUiChartLegend {
            source: GameUiReadoutSource::Static,
            label: marker.label.clone(),
            color: marker.color.clone(),
            segment_gradient: None,
        }))
        .chain(
            chart
                .vectors
                .iter()
                .enumerate()
                .filter(|(_, vector)| !vector.label.is_empty())
                .map(|(index, vector)| GameUiChartLegend {
                    source: if chart.equal_axis_scale && index == 0 {
                        GameUiReadoutSource::AnalysisCursor
                    } else {
                        GameUiReadoutSource::Static
                    },
                    label: if chart.equal_axis_scale && index == 0 {
                        vector.label.clone()
                    } else {
                        "周辺の風（発進高度・矢印は空気の移動先）".to_owned()
                    },
                    color: vector.color.clone(),
                    segment_gradient: None,
                }),
        )
        .collect();
    if chart.equal_axis_scale {
        let distance = scale_bar_distance(chart.x_maximum - chart.x_minimum);
        let scale_width = distance / (chart.x_maximum - chart.x_minimum) * width;
        let scale_x = left;
        let scale_y = 42.0 * scale;
        let north_x = right - 12.0 * scale;
        let north_top = 22.0 * scale;
        let north_length = 24.0 * scale;
        let north_head_height = north_length.min(10.0 * scale);
        let north_head_width = (6.0 * scale).min(north_x - left).min(right - north_x);
        presentation.map_guides = Some(GameUiChartMapGuides {
            scale_distance_m: distance,
            scale_segments: [
                [
                    point(scale_x, scale_y),
                    point(scale_x + scale_width, scale_y),
                ],
                [
                    point(scale_x, scale_y - 6.0 * scale),
                    point(scale_x, scale_y + 3.0 * scale),
                ],
                [
                    point(scale_x + scale_width, scale_y - 6.0 * scale),
                    point(scale_x + scale_width, scale_y + 3.0 * scale),
                ],
            ],
            scale_label: chart_text(
                format!("{} m", format_chart_tick(distance, f64::INFINITY)),
                point(scale_x + scale_width / 2.0, scale_y - 8.0 * scale),
                presentation.tick_font_size,
                600,
                GameUiChartTextAnchor::Middle,
                "#f3f4e8",
            ),
            north_segments: [
                [
                    point(north_x, north_top + north_length),
                    point(north_x, north_top),
                ],
                [
                    point(north_x, north_top),
                    point(north_x - north_head_width, north_top + north_head_height),
                ],
                [
                    point(north_x, north_top),
                    point(north_x + north_head_width, north_top + north_head_height),
                ],
            ],
            north_label: chart_text(
                "N".to_owned(),
                point(north_x, north_top - 6.0 * scale),
                presentation.tick_font_size,
                600,
                GameUiChartTextAnchor::Middle,
                "#f3f4e8",
            ),
        });
    }
}

fn chart_text(
    label: String,
    position: GameUiChartPoint,
    font_size: f64,
    font_weight: u16,
    anchor: GameUiChartTextAnchor,
    color: &str,
) -> GameUiChartText {
    GameUiChartText {
        label,
        position,
        font_size,
        font_weight,
        rotation_degrees: 0.0,
        anchor,
        color: color.to_owned(),
    }
}

fn format_chart_tick(value: f64, interval: f64) -> String {
    if !value.is_finite() {
        return String::new();
    }
    let magnitude = value.abs();
    let mut digits = if magnitude >= 100.0 {
        0
    } else if magnitude >= 10.0 {
        1
    } else {
        2
    };
    let mut buffer = ryu_js::Buffer::new();
    let mut resolution = 10.0_f64.powi(-i32::from(digits));
    while interval > 0.0 && digits < 100 && resolution > interval {
        digits += 1;
        resolution /= 10.0;
    }
    let raw = if interval <= 0.0 || interval.is_nan() || resolution > interval {
        buffer.format_finite(value)
    } else {
        buffer.format_to_fixed(value, digits)
    };
    let trimmed = if raw.contains('.') && !raw.contains('e') {
        raw.trim_end_matches('0').trim_end_matches('.')
    } else {
        raw
    };
    if trimmed == "-0" {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn scale_bar_distance(span: f64) -> f64 {
    let desired = span / 4.0;
    let magnitude = 10.0_f64.powf(desired.log10().floor());
    let normalized = desired / magnitude;
    let factor = if normalized >= 5.0 {
        5.0
    } else if normalized >= 2.0 {
        2.0
    } else {
        1.0
    };
    factor * magnitude
}

pub(super) fn chart_cursor(
    id: &str,
    kind: GameUiAnalysisChart,
    plot: &GameUiChart,
    sample: FlightRecordPlaybackSample,
    time: f64,
    record: &FlightRecord,
    aircraft: Option<GameUiRecordAircraft>,
) -> GameUiChartCursor {
    let bounds = [
        plot.x_minimum,
        plot.x_maximum,
        plot.y_minimum,
        plot.y_maximum,
    ];
    ChartProjection::new(
        bounds,
        plot.presentation.plot,
        plot.presentation.tick_font_size / BASE_TICK_FONT_PX,
    )
    .cursor(data_cursor(
        id,
        kind,
        bounds,
        &plot.series,
        sample,
        time,
        record,
        aircraft,
    ))
}

#[allow(clippy::too_many_arguments)]
fn data_cursor(
    id: &str,
    kind: GameUiAnalysisChart,
    bounds: [f64; 4],
    series: &[GameUiChartSeries],
    sample: FlightRecordPlaybackSample,
    time: f64,
    record: &FlightRecord,
    aircraft: Option<GameUiRecordAircraft>,
) -> ChartDataCursor {
    let mut cursor = ChartDataCursor {
        id: id.to_owned(),
        cursor_x: Some(time),
        cursor_points: Vec::new(),
        selected_wind_vector: None,
    };
    match kind {
        GameUiAnalysisChart::Map => {
            cursor.cursor_x = None;
            let position = sample.telemetry.composite_cg_position_ned_m.components();
            let start = point(position[1], position[0]);
            cursor.cursor_points.push(GameUiChartCursorPoint {
                point: start,
                color: series[0].color.clone(),
            });
            let [north, east, _] = sample.telemetry.wind_velocity_ned_mps.components();
            let speed = north.hypot(east);
            let half_range = (bounds[1] - bounds[0]) * 0.75 / 2.0;
            let scale = if speed > 0.0 {
                half_range * 0.2 / speed
            } else {
                0.0
            };
            let end = point(start.x + east * scale, start.y + north * scale);
            cursor.selected_wind_vector = Some(ChartWindVector {
                label: if speed > 0.0 {
                    format!("選択地点の風の矢印: 1 m/s あたり {scale:.1} m")
                } else {
                    "選択地点の水平風: 0.0 m/s".to_owned()
                },
                color: "#f2b35e".to_owned(),
                segment: if speed > 0.0 {
                    clip_segment(start, end, bounds)
                } else {
                    None
                },
            });
        }
        GameUiAnalysisChart::Altitude => cursor.cursor_points.push(GameUiChartCursorPoint {
            point: point(time, sample.telemetry.altitude_m),
            color: series[0].color.clone(),
        }),
        GameUiAnalysisChart::Speed => {
            cursor.cursor_points.push(GameUiChartCursorPoint {
                point: point(time, sample.telemetry.airspeed_mps),
                color: series[0].color.clone(),
            });
            cursor.cursor_points.push(GameUiChartCursorPoint {
                point: point(time, sample.telemetry.groundspeed_mps),
                color: series[1].color.clone(),
            });
        }
        _ => {
            let values = analysis_cursor_values(kind, record, aircraft, &sample, time);
            for (index, value) in values.into_iter().enumerate() {
                if let (Some(value), Some(definition)) = (value, series.get(index)) {
                    cursor.cursor_points.push(GameUiChartCursorPoint {
                        point: point(time, value),
                        color: definition.color.clone(),
                    });
                }
            }
        }
    }
    cursor
}

/// Whether a chart kind can be presented for a record and aircraft supply.
///
/// Charts that need a per-record diagnostic or the identified aircraft
/// parameters are hidden when their data is absent, following the Analysis
/// rule that additional quantities appear only for records carrying them.
pub(super) fn analysis_chart_available(
    kind: GameUiAnalysisChart,
    record: Option<&FlightRecord>,
    aircraft: Option<GameUiRecordAircraft>,
) -> bool {
    match kind {
        GameUiAnalysisChart::TailTorque => record.is_some_and(|record| {
            !record.samples().is_empty()
                && record
                    .samples()
                    .iter()
                    .all(|sample| sample.tail_wrenches.is_some())
        }),
        GameUiAnalysisChart::DragBreakdown => record.is_some_and(|record| {
            !record.samples().is_empty()
                && record
                    .samples()
                    .iter()
                    .all(|sample| sample.drag_breakdown.is_some())
        }),
        GameUiAnalysisChart::KineticEnergy
        | GameUiAnalysisChart::EnergyBalance
        | GameUiAnalysisChart::EnergyLossRate => aircraft.is_some(),
        GameUiAnalysisChart::LoadFactor => {
            aircraft.is_some_and(|supply| supply.gravity.meters_per_second_squared() > 0.0)
        }
        _ => true,
    }
}

/// One named value series over the retained samples; `None` leaves a gap.
struct SeriesSpec {
    label: &'static str,
    color: &'static str,
    values: Vec<Option<f64>>,
}

fn spec(label: &'static str, color: &'static str, values: Vec<Option<f64>>) -> SeriesSpec {
    SeriesSpec {
        label,
        color,
        values,
    }
}

fn fill_series(
    chart: &mut ChartData,
    record: &FlightRecord,
    y_axis_label: &str,
    specs: Vec<SeriesSpec>,
) {
    chart.y_axis_label = y_axis_label.to_owned();
    let values: Vec<f64> = specs
        .iter()
        .flat_map(|spec| spec.values.iter().filter_map(|value| *value))
        .collect();
    let range = if values.is_empty() {
        [0.0, 1.0]
    } else {
        padded_range(values.into_iter())
    };
    chart.y_minimum = range[0];
    chart.y_maximum = range[1];
    for spec in specs {
        chart.series.push(series(
            spec.label,
            spec.color,
            record
                .samples()
                .iter()
                .zip(spec.values)
                .filter_map(|(sample, value)| {
                    value.map(|value| point(sample_time(record, sample), value))
                })
                .collect(),
        ));
    }
}

/// Interpolates a per-sample derived quantity at an arbitrary record time,
/// using the bracketing retained samples.
fn derived_at(
    record: &FlightRecord,
    time: f64,
    value: impl Fn(usize) -> Option<f64>,
) -> Option<f64> {
    let samples = record.samples();
    if samples.is_empty() {
        return None;
    }
    let times: Vec<f64> = samples
        .iter()
        .map(|sample| sample_time(record, sample))
        .collect();
    if time <= times[0] {
        return value(0);
    }
    let last = samples.len() - 1;
    if time >= times[last] {
        return value(last);
    }
    let end = times.iter().position(|point| *point > time)?;
    let start = end - 1;
    let (first, second) = (value(start)?, value(end)?);
    let span = times[end] - times[start];
    if span <= 0.0 {
        return Some(first);
    }
    Some(first + (second - first) * (time - times[start]) / span)
}

/// Removes ±π wraps from an angle series so continuous rotation stays
/// continuous on the plot.
fn unwrap_series(values: &[f64]) -> Vec<f64> {
    let mut output = Vec::with_capacity(values.len());
    let mut offset = 0.0;
    let mut previous: Option<f64> = None;
    for &value in values {
        if let Some(previous) = previous {
            let delta = value - previous;
            if delta > core::f64::consts::PI {
                offset -= 2.0 * core::f64::consts::PI;
            } else if delta < -core::f64::consts::PI {
                offset += 2.0 * core::f64::consts::PI;
            }
        }
        output.push(value + offset);
        previous = Some(value);
    }
    output
}

/// Shifts a wrapped angle by whole turns to sit next to its unwrapped
/// series value at the same time.
fn unwrap_adjust(value: f64, reference: f64) -> f64 {
    value
        + ((reference - value) / (2.0 * core::f64::consts::PI)).round()
            * 2.0
            * core::f64::consts::PI
}

/// Composite center-of-mass offset from datum O in body FRD axes.
fn cg_offset_body(state: FlightState, telemetry: FlightTelemetry) -> Option<[f64; 3]> {
    let cg = telemetry.composite_cg_position_ned_m.components();
    let datum = state.datum_position_ned().components();
    let delta = NedVector::try_new(cg[0] - datum[0], cg[1] - datum[1], cg[2] - datum[2]).ok()?;
    state
        .attitude_body_to_ned()
        .ned_to_body(delta)
        .ok()
        .map(|vector| vector.components())
}

/// Mechanical energy of one state under the supplied aircraft parameters.
fn energy_at(supply: GameUiRecordAircraft, state: &FlightState) -> Option<FlightEnergy> {
    flight_energy(&supply.model, supply.gravity, state).ok()
}

/// Per-sample energies; empty when no aircraft supply is available.
fn energy_series(
    record: &FlightRecord,
    aircraft: Option<GameUiRecordAircraft>,
) -> Vec<Option<FlightEnergy>> {
    match aircraft {
        Some(supply) => record
            .samples()
            .iter()
            .map(|sample| energy_at(supply, &sample.flight_state))
            .collect(),
        None => record.samples().iter().map(|_| None).collect(),
    }
}

/// Load factors [body-z, magnitude] of one sample from its acceleration
/// estimate, under the supplied gravity.
fn load_factor_from(
    record: &FlightRecord,
    gravity: f64,
    index: usize,
    estimate: FlightRecordAcceleration,
) -> Option<[f64; 2]> {
    let [ax, ay, az] = estimate.datum_acceleration_ned_mps2;
    let specific = NedVector::try_new(ax, ay, az - gravity).ok()?;
    let body = record
        .samples()
        .get(index)?
        .flight_state
        .attitude_body_to_ned()
        .ned_to_body(specific)
        .ok()?
        .components();
    let magnitude = (ax * ax + ay * ay + (az - gravity) * (az - gravity)).sqrt();
    Some([-body[2] / gravity, magnitude / gravity])
}

/// Per-sample load factors [body-z, magnitude] under the supplied gravity,
/// sharing one acceleration-series build across the whole record.
fn load_factors(record: &FlightRecord, gravity: f64) -> Vec<Option<[f64; 2]>> {
    if gravity <= 0.0 {
        return record.samples().iter().map(|_| None).collect();
    }
    record
        .acceleration_estimates()
        .into_iter()
        .enumerate()
        .map(|(index, estimate)| {
            estimate.and_then(|estimate| load_factor_from(record, gravity, index, estimate))
        })
        .collect()
}

/// Incidence target of one recorded input: the mixed builtin target, or the
/// structural target of an external update.
fn incidence_target(
    input: birdman_game_core::FlightRecordTailInput,
) -> Option<birdman_game_core::TailIncidence> {
    match input.commands() {
        TailControlReport::BuiltIn(commands) => Some(commands.mixed_incidence_target()),
        TailControlReport::External(update) => Some(update.structural_target()),
    }
}

fn analysis_series_chart(
    chart: &mut ChartData,
    record: &FlightRecord,
    kind: GameUiAnalysisChart,
    aircraft: Option<GameUiRecordAircraft>,
) {
    let samples = record.samples();
    let column = |pick: &dyn Fn(&FlightRecordSample) -> Option<f64>| {
        samples.iter().map(pick).collect::<Vec<_>>()
    };
    match kind {
        GameUiAnalysisChart::Attitude => {
            let rolls = unwrap_series(
                &samples
                    .iter()
                    .map(|sample| sample.telemetry.roll_rad)
                    .collect::<Vec<_>>(),
            );
            let headings = unwrap_series(
                &samples
                    .iter()
                    .map(|sample| sample.telemetry.heading_rad)
                    .collect::<Vec<_>>(),
            );
            fill_series(
                chart,
                record,
                "角度 (rad)",
                vec![
                    spec("ロール角", "#70d6c8", rolls.into_iter().map(Some).collect()),
                    spec(
                        "ピッチ角",
                        "#f2b35e",
                        column(&|sample| Some(sample.telemetry.pitch_rad)),
                    ),
                    spec(
                        "方位角",
                        "#85c7e8",
                        headings.into_iter().map(Some).collect(),
                    ),
                ],
            );
        }
        GameUiAnalysisChart::AngularVelocity => {
            let rates: Vec<[f64; 3]> = samples
                .iter()
                .map(|sample| sample.flight_state.angular_velocity_body().components())
                .collect();
            fill_series(
                chart,
                record,
                "角速度 (rad/s)",
                vec![
                    spec(
                        "ロール角速度 p",
                        "#70d6c8",
                        rates.iter().map(|rate| Some(rate[0])).collect(),
                    ),
                    spec(
                        "ピッチ角速度 q",
                        "#f2b35e",
                        rates.iter().map(|rate| Some(rate[1])).collect(),
                    ),
                    spec(
                        "ヨー角速度 r",
                        "#85c7e8",
                        rates.iter().map(|rate| Some(rate[2])).collect(),
                    ),
                ],
            );
        }
        GameUiAnalysisChart::Acceleration => {
            let estimates = record.acceleration_estimates();
            let pick = |axis: usize, angular: bool| {
                estimates
                    .iter()
                    .map(|estimate| {
                        estimate.map(|estimate| {
                            if angular {
                                estimate.angular_acceleration_body_rad_s2[axis]
                            } else {
                                estimate.datum_acceleration_ned_mps2[axis]
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            };
            fill_series(
                chart,
                record,
                "加速度 (m/s²・rad/s²)",
                vec![
                    spec("線加速度 北", "#70d6c8", pick(0, false)),
                    spec("線加速度 東", "#f2b35e", pick(1, false)),
                    spec("線加速度 下", "#85c7e8", pick(2, false)),
                    spec("角加速度 p", "#f29c78", pick(0, true)),
                    spec("角加速度 q", "#c7eb72", pick(1, true)),
                    spec("角加速度 r", "#d2a8ff", pick(2, true)),
                ],
            );
        }
        GameUiAnalysisChart::FlowAngles => {
            fill_series(
                chart,
                record,
                "角度 (rad)",
                vec![
                    spec(
                        "迎え角",
                        "#70d6c8",
                        column(&|sample| sample.telemetry.angle_of_attack_rad),
                    ),
                    spec(
                        "横滑り角",
                        "#f2b35e",
                        column(&|sample| sample.telemetry.sideslip_angle_rad),
                    ),
                ],
            );
        }
        GameUiAnalysisChart::TailTorque => {
            const LABELS: [[&str; 3]; 2] = [
                [
                    "水平尾翼 ロールモーメント",
                    "水平尾翼 ピッチモーメント",
                    "水平尾翼 ヨーモーメント",
                ],
                [
                    "垂直尾翼 ロールモーメント",
                    "垂直尾翼 ピッチモーメント",
                    "垂直尾翼 ヨーモーメント",
                ],
            ];
            const COLORS: [[&str; 3]; 2] = [
                ["#70d6c8", "#85c7e8", "#c7eb72"],
                ["#f2b35e", "#f29c78", "#d2a8ff"],
            ];
            let mut specs = Vec::new();
            for tail in 0..2 {
                for axis in 0..3 {
                    specs.push(spec(
                        LABELS[tail][axis],
                        COLORS[tail][axis],
                        column(&move |sample| {
                            sample
                                .tail_wrenches
                                .and_then(|wrenches| wrenches[tail])
                                .map(|wrench| {
                                    wrench.moment_about_datum_newton_meters().components()[axis]
                                })
                        }),
                    ));
                }
            }
            fill_series(chart, record, "モーメント (N·m)", specs);
        }
        GameUiAnalysisChart::InputHistory => {
            let mut position_command: Vec<Option<f64>> = Vec::with_capacity(samples.len());
            let mut carried: Option<f64> = None;
            for sample in samples {
                if let Some(input) = sample.controls.input_from_previous
                    && let TailPilotPositionCommand::Set(intent) = input.pilot_position_command()
                {
                    carried = Some(intent.value());
                }
                position_command.push(carried);
            }
            let target = |axis: usize| {
                samples
                    .iter()
                    .map(|sample| {
                        sample
                            .controls
                            .input_from_previous
                            .and_then(incidence_target)
                            .map(|incidence| {
                                if axis == 0 {
                                    incidence.elevator_rad()
                                } else {
                                    incidence.rudder_rad()
                                }
                            })
                    })
                    .collect::<Vec<_>>()
            };
            fill_series(
                chart,
                record,
                "入力・指令",
                vec![
                    spec(
                        "意図 nose-up",
                        "#70d6c8",
                        column(&|sample| {
                            sample
                                .controls
                                .input_from_previous
                                .map(|input| input.manual_intent().nose_up())
                        }),
                    ),
                    spec(
                        "意図 turn-right",
                        "#85c7e8",
                        column(&|sample| {
                            sample
                                .controls
                                .input_from_previous
                                .map(|input| input.manual_intent().turn_right())
                        }),
                    ),
                    spec(
                        "目標角速度 q",
                        "#f2b35e",
                        column(&|sample| {
                            sample
                                .controls
                                .input_from_previous
                                .map(|input| input.desired_body_rate().pitch_rad_per_second())
                        }),
                    ),
                    spec(
                        "目標角速度 r",
                        "#ffa657",
                        column(&|sample| {
                            sample
                                .controls
                                .input_from_previous
                                .map(|input| input.desired_body_rate().yaw_rad_per_second())
                        }),
                    ),
                    spec("尾翼角目標 水平", "#c7eb72", target(0)),
                    spec("尾翼角目標 垂直", "#7ee787", target(1)),
                    spec(
                        "実尾翼角 水平",
                        "#f29c78",
                        column(&|sample| Some(sample.controls.incidence.elevator_rad())),
                    ),
                    spec(
                        "実尾翼角 垂直",
                        "#ff7b72",
                        column(&|sample| Some(sample.controls.incidence.rudder_rad())),
                    ),
                    spec("パイロット位置指令", "#d2a8ff", position_command),
                ],
            );
        }
        GameUiAnalysisChart::CenterOfGravity => {
            let offsets: Vec<Option<[f64; 3]>> = samples
                .iter()
                .map(|sample| cg_offset_body(sample.flight_state, sample.telemetry))
                .collect();
            fill_series(
                chart,
                record,
                "位置 (m)",
                vec![
                    spec(
                        "重心オフセット x",
                        "#70d6c8",
                        offsets
                            .iter()
                            .map(|offset| offset.map(|value| value[0]))
                            .collect(),
                    ),
                    spec(
                        "重心オフセット y",
                        "#f2b35e",
                        offsets
                            .iter()
                            .map(|offset| offset.map(|value| value[1]))
                            .collect(),
                    ),
                    spec(
                        "重心オフセット z",
                        "#85c7e8",
                        offsets
                            .iter()
                            .map(|offset| offset.map(|value| value[2]))
                            .collect(),
                    ),
                    spec(
                        "パイロット位置",
                        "#f29c78",
                        column(&|sample| Some(sample.flight_state.pilot_position_m())),
                    ),
                ],
            );
        }
        GameUiAnalysisChart::DragBreakdown => {
            fill_series(
                chart,
                record,
                "抗力 (N)",
                vec![
                    spec(
                        "誘導抗力",
                        "#f2b35e",
                        column(&|sample| sample.drag_breakdown.map(|drag| drag.induced_newtons())),
                    ),
                    spec(
                        "形状抗力",
                        "#70d6c8",
                        column(&|sample| sample.drag_breakdown.map(|drag| drag.profile_newtons())),
                    ),
                    spec(
                        "抗力合計",
                        "#e8e6df",
                        column(&|sample| sample.drag_breakdown.map(|drag| drag.total_newtons())),
                    ),
                ],
            );
        }
        GameUiAnalysisChart::KineticEnergy => {
            let energies = energy_series(record, aircraft);
            fill_series(
                chart,
                record,
                "エネルギー (J)",
                vec![spec(
                    "運動エネルギー",
                    "#70d6c8",
                    energies
                        .iter()
                        .map(|energy| energy.map(|energy| energy.kinetic_j()))
                        .collect(),
                )],
            );
        }
        GameUiAnalysisChart::EnergyBalance => {
            let energies = energy_series(record, aircraft);
            let initial_total = energies
                .first()
                .copied()
                .flatten()
                .map(|energy| energy.total_j());
            fill_series(
                chart,
                record,
                "エネルギー (J)",
                vec![
                    spec(
                        "位置エネルギー",
                        "#85c7e8",
                        energies
                            .iter()
                            .map(|energy| energy.map(|energy| energy.potential_j()))
                            .collect(),
                    ),
                    spec(
                        "運動エネルギー",
                        "#70d6c8",
                        energies
                            .iter()
                            .map(|energy| energy.map(|energy| energy.kinetic_j()))
                            .collect(),
                    ),
                    spec(
                        "合計エネルギー",
                        "#e8e6df",
                        energies
                            .iter()
                            .map(|energy| energy.map(|energy| energy.total_j()))
                            .collect(),
                    ),
                    spec(
                        "累積損失",
                        "#f29c78",
                        energies
                            .iter()
                            .map(|energy| match (initial_total, energy) {
                                (Some(initial), Some(energy)) => Some(initial - energy.total_j()),
                                _ => None,
                            })
                            .collect(),
                    ),
                ],
            );
        }
        GameUiAnalysisChart::EnergyLossRate => {
            let energies = energy_series(record, aircraft);
            let totals: Option<Vec<f64>> = energies
                .iter()
                .map(|energy| energy.map(|energy| energy.total_j()))
                .collect();
            let values: Vec<Option<f64>> = match &totals {
                Some(totals) => record
                    .series_derivatives(totals)
                    .map(|derivatives| {
                        derivatives
                            .into_iter()
                            .map(|rate| rate.map(|rate| -rate))
                            .collect()
                    })
                    .unwrap_or_else(|| samples.iter().map(|_| None).collect()),
                None => samples.iter().map(|_| None).collect(),
            };
            fill_series(
                chart,
                record,
                "損失率 (W)",
                vec![spec("エネルギー損失率", "#f2b35e", values)],
            );
        }
        GameUiAnalysisChart::SinkAndGlide => {
            fill_series(
                chart,
                record,
                "沈降速度 (m/s)・滑空比",
                vec![
                    spec(
                        "沈降速度",
                        "#70d6c8",
                        column(&|sample| {
                            Some(sample.flight_state.datum_velocity_ned().components()[2])
                        }),
                    ),
                    spec(
                        "滑空比",
                        "#f2b35e",
                        column(&|sample| {
                            let velocity = sample.flight_state.datum_velocity_ned().components();
                            (velocity[2] > 0.05)
                                .then(|| velocity[0].hypot(velocity[1]) / velocity[2])
                        }),
                    ),
                ],
            );
        }
        GameUiAnalysisChart::LoadFactor => {
            let gravity = aircraft.map_or(0.0, |supply| supply.gravity.meters_per_second_squared());
            let factors = load_factors(record, gravity);
            fill_series(
                chart,
                record,
                "荷重倍数",
                vec![
                    spec(
                        "荷重倍数（機体z軸）",
                        "#70d6c8",
                        factors
                            .iter()
                            .map(|factor| factor.map(|value| value[0]))
                            .collect(),
                    ),
                    spec(
                        "荷重倍数（大きさ）",
                        "#f2b35e",
                        factors
                            .iter()
                            .map(|factor| factor.map(|value| value[1]))
                            .collect(),
                    ),
                ],
            );
        }
        GameUiAnalysisChart::Wind => {
            fill_series(
                chart,
                record,
                "風速 (m/s)",
                vec![
                    spec(
                        "北向きの風",
                        "#70d6c8",
                        column(&|sample| {
                            Some(sample.telemetry.wind_velocity_ned_mps.components()[0])
                        }),
                    ),
                    spec(
                        "東向きの風",
                        "#f2b35e",
                        column(&|sample| {
                            Some(sample.telemetry.wind_velocity_ned_mps.components()[1])
                        }),
                    ),
                    spec(
                        "下向きの風",
                        "#85c7e8",
                        column(&|sample| {
                            Some(sample.telemetry.wind_velocity_ned_mps.components()[2])
                        }),
                    ),
                ],
            );
        }
        _ => {}
    }
}

/// Cursor values for the series charts, aligned with the series order of
/// [`analysis_series_chart`].
fn analysis_cursor_values(
    kind: GameUiAnalysisChart,
    record: &FlightRecord,
    aircraft: Option<GameUiRecordAircraft>,
    sample: &FlightRecordPlaybackSample,
    time: f64,
) -> Vec<Option<f64>> {
    match kind {
        GameUiAnalysisChart::Attitude => {
            let rolls = unwrap_series(
                &record
                    .samples()
                    .iter()
                    .map(|sample| sample.telemetry.roll_rad)
                    .collect::<Vec<_>>(),
            );
            let headings = unwrap_series(
                &record
                    .samples()
                    .iter()
                    .map(|sample| sample.telemetry.heading_rad)
                    .collect::<Vec<_>>(),
            );
            let adjust = |value: f64, series: &[f64]| {
                derived_at(record, time, |index| series.get(index).copied())
                    .map(|reference| unwrap_adjust(value, reference))
                    .or(Some(value))
            };
            vec![
                adjust(sample.telemetry.roll_rad, &rolls),
                Some(sample.telemetry.pitch_rad),
                adjust(sample.telemetry.heading_rad, &headings),
            ]
        }
        GameUiAnalysisChart::AngularVelocity => sample
            .flight_state
            .angular_velocity_body()
            .components()
            .into_iter()
            .map(Some)
            .collect(),
        GameUiAnalysisChart::Acceleration => {
            let estimates = record.acceleration_estimates();
            let mut values = Vec::with_capacity(6);
            for axis in 0..3 {
                values.push(derived_at(record, time, |index| {
                    estimates
                        .get(index)
                        .copied()
                        .flatten()
                        .map(|estimate| estimate.datum_acceleration_ned_mps2[axis])
                }));
            }
            for axis in 0..3 {
                values.push(derived_at(record, time, |index| {
                    estimates
                        .get(index)
                        .copied()
                        .flatten()
                        .map(|estimate| estimate.angular_acceleration_body_rad_s2[axis])
                }));
            }
            values
        }
        GameUiAnalysisChart::FlowAngles => vec![
            sample.telemetry.angle_of_attack_rad,
            sample.telemetry.sideslip_angle_rad,
        ],
        GameUiAnalysisChart::TailTorque => {
            let mut values = Vec::with_capacity(6);
            for tail in 0..2 {
                for axis in 0..3 {
                    values.push(
                        sample
                            .tail_wrenches
                            .and_then(|wrenches| wrenches[tail])
                            .map(|wrench| {
                                wrench.moment_about_datum_newton_meters().components()[axis]
                            }),
                    );
                }
            }
            values
        }
        GameUiAnalysisChart::InputHistory => {
            let input = sample.input;
            let target = input.and_then(incidence_target);
            let mut position_command = None;
            for stored in record.samples() {
                if sample_time(record, stored) > time {
                    break;
                }
                if let Some(stored_input) = stored.controls.input_from_previous
                    && let TailPilotPositionCommand::Set(intent) =
                        stored_input.pilot_position_command()
                {
                    position_command = Some(intent.value());
                }
            }
            vec![
                input.map(|input| input.manual_intent().nose_up()),
                input.map(|input| input.manual_intent().turn_right()),
                input.map(|input| input.desired_body_rate().pitch_rad_per_second()),
                input.map(|input| input.desired_body_rate().yaw_rad_per_second()),
                target.map(|incidence| incidence.elevator_rad()),
                target.map(|incidence| incidence.rudder_rad()),
                Some(sample.actuators.elevator_rad()),
                Some(sample.actuators.rudder_rad()),
                position_command,
            ]
        }
        GameUiAnalysisChart::CenterOfGravity => {
            let offset = cg_offset_body(sample.flight_state, sample.telemetry);
            vec![
                offset.map(|value| value[0]),
                offset.map(|value| value[1]),
                offset.map(|value| value[2]),
                Some(sample.flight_state.pilot_position_m()),
            ]
        }
        GameUiAnalysisChart::DragBreakdown => vec![
            sample.drag_breakdown.map(|drag| drag.induced_newtons()),
            sample.drag_breakdown.map(|drag| drag.profile_newtons()),
            sample.drag_breakdown.map(|drag| drag.total_newtons()),
        ],
        GameUiAnalysisChart::KineticEnergy => {
            vec![
                aircraft
                    .and_then(|supply| energy_at(supply, &sample.flight_state))
                    .map(|energy| energy.kinetic_j()),
            ]
        }
        GameUiAnalysisChart::EnergyBalance => {
            let current = aircraft.and_then(|supply| energy_at(supply, &sample.flight_state));
            let initial = aircraft.and_then(|supply| {
                record
                    .samples()
                    .first()
                    .and_then(|first| energy_at(supply, &first.flight_state))
            });
            vec![
                current.map(|energy| energy.potential_j()),
                current.map(|energy| energy.kinetic_j()),
                current.map(|energy| energy.total_j()),
                match (initial, current) {
                    (Some(initial), Some(current)) => Some(initial.total_j() - current.total_j()),
                    _ => None,
                },
            ]
        }
        GameUiAnalysisChart::EnergyLossRate => {
            let energies = energy_series(record, aircraft);
            let totals: Option<Vec<f64>> = energies
                .iter()
                .map(|energy| energy.map(|energy| energy.total_j()))
                .collect();
            let derivatives = totals
                .as_ref()
                .and_then(|totals| record.series_derivatives(totals));
            vec![match &derivatives {
                Some(derivatives) => derived_at(record, time, |index| {
                    derivatives.get(index).copied().flatten().map(|rate| -rate)
                }),
                None => None,
            }]
        }
        GameUiAnalysisChart::SinkAndGlide => {
            let velocity = sample.flight_state.datum_velocity_ned().components();
            vec![
                Some(velocity[2]),
                (velocity[2] > 0.05).then(|| velocity[0].hypot(velocity[1]) / velocity[2]),
            ]
        }
        GameUiAnalysisChart::LoadFactor => {
            let gravity = aircraft.map_or(0.0, |supply| supply.gravity.meters_per_second_squared());
            let factors = load_factors(record, gravity);
            let factor = derived_at(record, time, |index| {
                factors.get(index).copied().flatten().map(|value| value[0])
            });
            let magnitude = derived_at(record, time, |index| {
                factors.get(index).copied().flatten().map(|value| value[1])
            });
            vec![factor, magnitude]
        }
        GameUiAnalysisChart::Wind => sample
            .telemetry
            .wind_velocity_ned_mps
            .components()
            .into_iter()
            .map(Some)
            .collect(),
        _ => Vec::new(),
    }
}

fn map_chart(chart: &mut ChartData, record: &FlightRecord) -> Result<(), GameUiError> {
    let samples = record.samples();
    let points: Vec<_> = samples.iter().map(map_point).collect();
    let east = range(points.iter().map(|position| position.x));
    let north = range(points.iter().map(|position| position.y));
    let center_east = (east[0] + east[1]) / 2.0;
    let center_north = (north[0] + north[1]) / 2.0;
    let half_range = (east[1] - east[0]).max(north[1] - north[0]).max(2.0) / 2.0;
    let bounds = [
        center_east - half_range / 0.75,
        center_east + half_range / 0.75,
        center_north - half_range / 0.75,
        center_north + half_range / 0.75,
    ];
    chart.x_axis_label = "東 E (m)".to_owned();
    chart.y_axis_label = "北 N (m)".to_owned();
    chart.x_minimum = bounds[0];
    chart.x_maximum = bounds[1];
    chart.y_minimum = bounds[2];
    chart.y_maximum = bounds[3];
    chart.equal_axis_scale = true;
    chart.cursor_x = None;
    let colors = (0..samples.len().saturating_sub(1))
        .map(|index| {
            progress_color(if samples.len() <= 2 {
                0.5
            } else {
                index as f64 / (samples.len() - 2) as f64
            })
        })
        .collect();
    chart.series.push(GameUiChartSeries {
        label: "飛行軌跡（紫=開始、黄=終端）".to_owned(),
        color: progress_color(0.0),
        points: points.clone(),
        segment_colors: colors,
    });
    let terminal = record.finalization().map_or("終了", |value| {
        super::game_ui_result_reason_label(value.reason)
    });
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        chart.markers.push(marker("Start", "#f3f4e8", *first));
        chart.markers.push(marker(terminal, "#f29c78", *last));
    }
    let duration = record.duration_seconds().map_err(GameUiError::Record)?;
    let interval = if duration <= 12.0 { 1.0 } else { 5.0 };
    let mut marker_time = interval;
    while marker_time < duration {
        if let Some(sample) = samples
            .iter()
            .find(|sample| sample_time(record, sample) >= marker_time)
        {
            chart.time_markers.push(marker(
                &format!("{marker_time} s"),
                "#d5e0dc",
                map_point(sample),
            ));
        }
        marker_time += interval;
    }
    wind_grid(chart, record, center_north, center_east, half_range, bounds)?;
    if environment::registered(record.header().scenario)
        && record.header().scenario.environment_version == 6
    {
        venue(chart, bounds)?;
    }
    Ok(())
}

fn wind_grid(
    chart: &mut ChartData,
    record: &FlightRecord,
    north: f64,
    east: f64,
    half_range: f64,
    bounds: [f64; 4],
) -> Result<(), GameUiError> {
    let Some(wind) =
        environment::registered_wind(record.header().scenario).map_err(GameUiError::Environment)?
    else {
        return Ok(());
    };
    let altitude = record
        .samples()
        .first()
        .map_or(0.0, |sample| sample.telemetry.altitude_m.max(0.0));
    let mut samples = Vec::with_capacity(25);
    for row in 0..5 {
        for column in 0..5 {
            let position = NedPoint::try_new(
                north - half_range + f64::from(row) * half_range / 2.0,
                east - half_range + f64::from(column) * half_range / 2.0,
                -altitude,
            )
            .map_err(|_| GameUiError::InvalidTime)?;
            let velocity = match wind.velocity_at(position) {
                Ok(velocity) => velocity.components(),
                Err(WindError::OutsideGrid) => return Ok(()),
                Err(error) => {
                    return Err(GameUiError::Environment(
                        birdman_game_format::EnvironmentFormatError::Wind(error),
                    ));
                }
            };
            samples.push((position.components(), velocity));
        }
    }
    let maximum_speed = samples
        .iter()
        .map(|(_, velocity)| velocity[0].hypot(velocity[1]))
        .fold(0.0, f64::max);
    let scale = if maximum_speed > 0.0 {
        half_range * 0.12 / maximum_speed
    } else {
        0.0
    };
    for (index, (position, velocity)) in samples.into_iter().enumerate() {
        let start = point(position[1], position[0]);
        let end = point(start.x + velocity[1] * scale, start.y + velocity[0] * scale);
        chart.vectors.push(ChartWindVector {
            label: if index == 0 {
                "周辺の風".to_owned()
            } else {
                String::new()
            },
            color: "#c7eb72".to_owned(),
            segment: if scale > 0.0 && velocity[0].hypot(velocity[1]) > 0.0 {
                clip_segment(start, end, bounds)
            } else {
                None
            },
        });
    }
    Ok(())
}

struct VenueGeometry {
    lines: Vec<GameUiChartSeries>,
    markers: Vec<GameUiChartMarker>,
}
static VENUE: OnceLock<Result<VenueGeometry, GameUiError>> = OnceLock::new();

fn venue(chart: &mut ChartData, bounds: [f64; 4]) -> Result<(), GameUiError> {
    let geometry = VENUE
        .get_or_init(build_venue)
        .as_ref()
        .map_err(|error| *error)?;
    for line in &geometry.lines {
        let mut fragments = Vec::new();
        let mut current = Vec::new();
        for pair in line.points.windows(2) {
            let Some([start, end]) = clip_segment(pair[0], pair[1], bounds) else {
                if current.len() > 1 {
                    fragments.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                continue;
            };
            if current.last().is_some_and(|last: &GameUiChartPoint| {
                (last.x - start.x).hypot(last.y - start.y) > 1e-8
            }) {
                if current.len() > 1 {
                    fragments.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
            if current.last() != Some(&start) {
                current.push(start);
            }
            current.push(end);
            if (end.x - pair[1].x).hypot(end.y - pair[1].y) > 1e-8 && current.len() > 1 {
                fragments.push(std::mem::take(&mut current));
            }
        }
        if current.len() > 1 {
            fragments.push(current);
        }
        for (index, points) in fragments.into_iter().enumerate() {
            chart.series.push(series(
                if index == 0 { &line.label } else { "" },
                &line.color,
                points,
            ));
        }
    }
    chart.markers.extend(
        geometry
            .markers
            .iter()
            .filter(|marker| inside(marker.point, bounds))
            .cloned(),
    );
    Ok(())
}

fn build_venue() -> Result<VenueGeometry, GameUiError> {
    let catalog = birdman_game_app::registered_venue().map_err(|_| GameUiError::InvalidVenueMap)?;
    let map = &catalog.analysis;
    let features = &catalog.features;
    let to_points = |positions: &[[f64; 2]]| {
        positions
            .iter()
            .map(|position| point(position[1], position[0]))
            .collect()
    };
    let mut lines: Vec<_> = map
        .shorelines_north_east_meters
        .iter()
        .enumerate()
        .map(|(index, positions)| {
            series(
                if index == 0 {
                    "彦根 湖岸線（OSM）"
                } else {
                    "彦根 湖岸線（OSM・続き）"
                },
                "#77b6c8",
                to_points(positions),
            )
        })
        .collect();
    let mut markers = Vec::new();
    for feature in &features.features {
        let beach = feature.kind == "beach";
        if matches!(feature.kind.as_str(), "pier" | "quay" | "breakwater")
            || (beach && feature.name.as_deref() == Some("松原水泳場"))
        {
            let fallback = match feature.kind.as_str() {
                "pier" => "桟橋（OSM）",
                "quay" => "岸壁（OSM）",
                "breakwater" => "防波堤（OSM）",
                _ => "砂浜（OSM）",
            };
            lines.push(series(
                feature.name.as_deref().unwrap_or(fallback),
                if beach { "#d8c89b" } else { "#a08f70" },
                to_points(&feature.north_east_meters),
            ));
        }
        if beach
            && let Some(name) = &feature.name
            && !feature.north_east_meters.is_empty()
        {
            let count = feature.north_east_meters.len() as f64;
            markers.push(marker(
                name,
                "#d8c89b",
                point(
                    feature
                        .north_east_meters
                        .iter()
                        .map(|position| position[1])
                        .sum::<f64>()
                        / count,
                    feature
                        .north_east_meters
                        .iter()
                        .map(|position| position[0])
                        .sum::<f64>()
                        / count,
                ),
            ));
        }
    }
    let platform = birdman_game_app::launch_venue()
        .map_err(|_| GameUiError::InvalidVenueMap)?
        .platform;
    let [forward_north, forward_east] = platform.horizontal_direction_ned();
    let positions = [
        (0.0, -1.0),
        (0.0, 1.0),
        (-platform.length_meters, 1.0),
        (-platform.length_meters, -1.0),
        (0.0, -1.0),
    ]
    .into_iter()
    .map(|(along, across)| {
        point(
            along * forward_east + across * platform.width_meters / 2.0 * forward_north,
            along * forward_north - across * platform.width_meters / 2.0 * forward_east,
        )
    })
    .collect();
    lines.push(series("発進台", "#e7c27b", positions));
    if lines
        .iter()
        .flat_map(|line| &line.points)
        .chain(markers.iter().map(|marker| &marker.point))
        .any(|position| !position.x.is_finite() || !position.y.is_finite())
    {
        return Err(GameUiError::InvalidVenueMap);
    }
    Ok(VenueGeometry { lines, markers })
}

fn clip_segment(
    start: GameUiChartPoint,
    end: GameUiChartPoint,
    bounds: [f64; 4],
) -> Option<[GameUiChartPoint; 2]> {
    let delta_x = end.x - start.x;
    let delta_y = end.y - start.y;
    let mut lower: f64 = 0.0;
    let mut upper: f64 = 1.0;
    for (direction, distance) in [
        (-delta_x, start.x - bounds[0]),
        (delta_x, bounds[1] - start.x),
        (-delta_y, start.y - bounds[2]),
        (delta_y, bounds[3] - start.y),
    ] {
        if direction == 0.0 {
            if distance < 0.0 {
                return None;
            }
            continue;
        }
        let ratio = distance / direction;
        if direction < 0.0 {
            lower = lower.max(ratio);
        } else {
            upper = upper.min(ratio);
        }
        if lower > upper {
            return None;
        }
    }
    Some([
        point(start.x + lower * delta_x, start.y + lower * delta_y),
        point(start.x + upper * delta_x, start.y + upper * delta_y),
    ])
}

fn inside(position: GameUiChartPoint, bounds: [f64; 4]) -> bool {
    position.x >= bounds[0]
        && position.x <= bounds[1]
        && position.y >= bounds[2]
        && position.y <= bounds[3]
}
fn map_point(sample: &FlightRecordSample) -> GameUiChartPoint {
    let position = sample.telemetry.composite_cg_position_ned_m.components();
    point(position[1], position[0])
}
fn sample_time(record: &FlightRecord, sample: &FlightRecordSample) -> f64 {
    (sample.tick_index as f64 + sample.fraction) / f64::from(record.header().physics_hz)
}
fn point(x: f64, y: f64) -> GameUiChartPoint {
    GameUiChartPoint { x, y }
}
fn series(label: &str, color: &str, points: Vec<GameUiChartPoint>) -> GameUiChartSeries {
    GameUiChartSeries {
        label: label.to_owned(),
        color: color.to_owned(),
        points,
        segment_colors: Vec::new(),
    }
}
fn marker(label: &str, color: &str, point: GameUiChartPoint) -> GameUiChartMarker {
    GameUiChartMarker {
        label: label.to_owned(),
        color: color.to_owned(),
        point,
    }
}
fn range(values: impl Iterator<Item = f64>) -> [f64; 2] {
    values.fold([f64::INFINITY, f64::NEG_INFINITY], |range, value| {
        [range[0].min(value), range[1].max(value)]
    })
}
fn padded_range(values: impl Iterator<Item = f64>) -> [f64; 2] {
    let range = range(values);
    let padding = ((range[1] - range[0]) * 0.08)
        .max(range[1].abs() * 0.02)
        .max(0.5);
    [range[0] - padding, range[1] + padding]
}
fn progress_color(progress: f64) -> String {
    let progress = progress.clamp(0.0, 1.0);
    let lower = if progress <= 0.5 {
        [68.0, 1.0, 84.0]
    } else {
        [33.0, 145.0, 140.0]
    };
    let upper = if progress <= 0.5 {
        [33.0, 145.0, 140.0]
    } else {
        [253.0, 231.0, 37.0]
    };
    let ratio = if progress <= 0.5 {
        progress * 2.0
    } else {
        (progress - 0.5) * 2.0
    };
    let channels: [u8; 3] = core::array::from_fn(|index| {
        (lower[index] + (upper[index] - lower[index]) * ratio).round() as u8
    });
    format!("#{:02x}{:02x}{:02x}", channels[0], channels[1], channels[2])
}

#[cfg(test)]
mod presentation_tests {
    use super::*;

    #[test]
    fn tick_labels_preserve_ecmascript_fixed_rounding_and_negative_zero() {
        for (value, expected) in [
            (0.0, "0"),
            (-0.0, "0"),
            (-0.001, "0"),
            (0.125, "0.13"),
            (-0.125, "-0.13"),
            (2.675, "2.67"),
            (10.25, "10.3"),
            (-10.25, "-10.3"),
            (100.5, "101"),
            (1.0e21, "1e+21"),
            (1.2e30, "1.2e+30"),
        ] {
            assert_eq!(format_chart_tick(value, f64::INFINITY), expected);
        }
        assert_eq!(format_chart_tick(0.125, 0.0), "0.125");
        assert_eq!(format_chart_tick(1.25e-110, 1.0e-111), "1.25e-110");
        for value in [-100.125, -0.125, 0.0, 0.125, 100.125] {
            assert_eq!(format_chart_tick(value, 0.001), value.to_string());
        }
    }

    #[test]
    fn map_scale_uses_the_web_one_two_five_policy() {
        for (span, expected) in [
            (4.0, 1.0),
            (8.0, 2.0),
            (20.0, 5.0),
            (100.0, 20.0),
            (1000.0, 200.0),
        ] {
            assert_eq!(scale_bar_distance(span), expected);
        }
    }

    #[test]
    fn venue_projection_uses_registered_shorelines_features_and_platform_geometry() {
        let catalog = birdman_game_app::registered_venue().unwrap();
        let geometry = build_venue().unwrap();
        let shoreline = &geometry.lines[0].points;
        for (point, [north, east]) in shoreline
            .iter()
            .zip(&catalog.analysis.shorelines_north_east_meters[0])
        {
            assert_eq!([point.x, point.y], [*east, *north]);
        }
        assert!(
            geometry
                .markers
                .iter()
                .any(|marker| marker.label == "松原水泳場")
        );
        let corners = &geometry.lines.last().unwrap().points;
        assert_eq!(corners.len(), 5);
        assert_eq!(corners.first(), corners.last());
        let platform = birdman_game_app::launch_venue().unwrap().platform;
        let distance = |first: &GameUiChartPoint, second: &GameUiChartPoint| {
            (first.x - second.x).hypot(first.y - second.y)
        };
        assert!((distance(&corners[0], &corners[1]) - platform.width_meters).abs() < 1e-10);
        assert!((distance(&corners[1], &corners[2]) - platform.length_meters).abs() < 1e-10);
        assert!(corners[0].y + corners[1].y > corners[2].y + corners[3].y);
        assert!(corners[0].x + corners[1].x < corners[2].x + corners[3].x);
    }
}
