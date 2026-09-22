import { describe, expect, test } from 'bun:test';

import type { Graph, HardwareInventory, NodeInstance } from './api';
import { deviceEdgeId, deviceLinks, deviceRoles } from './graph';

const inventory = (overrides: Partial<HardwareInventory> = {}): HardwareInventory => ({
  backend: 'test',
  sensors: [
    { id: 'chip/temp/cpu', label: 'CPU', quantity: 'temperature' },
    { id: 'chip/fan/1', label: 'Front intake speed', quantity: 'rpm' },
    { id: 'chip/fan/2', label: 'Rear exhaust speed', quantity: 'rpm' },
  ],
  channels: [
    {
      id: 'chip/pwm/1',
      label: 'Front intake',
      tachometer: 'chip/fan/1',
      minReliableDuty: null,
    },
    { id: 'chip/pwm/9', label: 'Headerless', tachometer: null, minReliableDuty: null },
  ],
  driverPresent: true,
  driverSummary: 'ok',
  moduleMissing: false,
  ...overrides,
});

const fan = (channel: string): NodeInstance => ({
  kind: { kind: 'fan-output', channel } as NodeInstance['kind'],
  label: '',
  position: [0, 0],
});

const sensorNode = (sensorId: string): NodeInstance => ({
  kind: {
    kind: 'sensor',
    sensor_id: sensorId,
    quantity: 'rpm',
  } as NodeInstance['kind'],
  label: '',
  position: [0, 0],
});

const graphOf = (nodes: Record<string, NodeInstance>): Graph => ({ nodes, edges: [] });

describe('same-device links', () => {
  test('pairs a fan output with the sensor reading its tachometer', () => {
    const g = graphOf({ f1: fan('chip/pwm/1'), tach: sensorNode('chip/fan/1') });
    const links = deviceLinks(g, inventory());

    expect(links).toHaveLength(1);
    expect(links[0]).toMatchObject({
      fanNodeId: 'f1',
      sensorNodeId: 'tach',
      channelId: 'chip/pwm/1',
      sensorId: 'chip/fan/1',
    });
  });

  test('ignores a sensor that is not this channel tachometer', () => {
    // A different fan's tach must not be drawn as belonging to this header.
    const g = graphOf({ f1: fan('chip/pwm/1'), other: sensorNode('chip/fan/2') });
    expect(deviceLinks(g, inventory())).toHaveLength(0);
  });

  test('a header with no tachometer links to nothing', () => {
    const g = graphOf({ f9: fan('chip/pwm/9'), tach: sensorNode('chip/fan/1') });
    expect(deviceLinks(g, inventory())).toHaveLength(0);
  });

  test('an unconfigured fan output links to nothing', () => {
    // Freshly dropped from the palette, before a channel is picked.
    const g = graphOf({ f1: fan(''), tach: sensorNode('chip/fan/1') });
    expect(deviceLinks(g, inventory())).toHaveLength(0);
  });

  test('links only appear once the tachometer has a node of its own', () => {
    // The tach is a separate source node, so driving a header is not by itself enough.
    const g = graphOf({ f1: fan('chip/pwm/1') });
    expect(deviceLinks(g, inventory())).toHaveLength(0);
  });

  test('two fan nodes on the same channel both link to the tachometer', () => {
    const g = graphOf({
      f1: fan('chip/pwm/1'),
      f2: fan('chip/pwm/1'),
      tach: sensorNode('chip/fan/1'),
    });
    expect(deviceLinks(g, inventory())).toHaveLength(2);
  });

  test('without an inventory there is nothing to pair by', () => {
    // The hardware is the only thing that knows a header and a tach belong together.
    const g = graphOf({ f1: fan('chip/pwm/1'), tach: sensorNode('chip/fan/1') });
    expect(deviceLinks(g, null)).toHaveLength(0);
  });

  test('roles put the handle on both ends, fan as source', () => {
    const g = graphOf({ f1: fan('chip/pwm/1'), tach: sensorNode('chip/fan/1') });
    const roles = deviceRoles(deviceLinks(g, inventory()));

    expect(roles.get('f1')).toBe('source');
    expect(roles.get('tach')).toBe('target');
    expect(roles.size).toBe(2);
  });

  test('edge ids are stable and distinct per pair', () => {
    const g = graphOf({
      f1: fan('chip/pwm/1'),
      f2: fan('chip/pwm/1'),
      tach: sensorNode('chip/fan/1'),
    });
    const ids = deviceLinks(g, inventory()).map(deviceEdgeId);

    expect(new Set(ids).size).toBe(ids.length);
    // Stable across calls, so React Flow does not remount the edge every render.
    expect(deviceLinks(g, inventory()).map(deviceEdgeId)).toEqual(ids);
  });
});
