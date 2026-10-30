import test from 'node:test';
import assert from 'node:assert/strict';
import { PET_FRAMES, PET_H, PET_W, petMood, petPixels } from './pet.js';

test('对话状态对应小鱼的样子', () => {
  assert.equal(petMood('detached'), null);
  assert.equal(petMood('running'), 'work');
  assert.equal(petMood('approval'), 'wait');
  assert.equal(petMood('idle', { doneAt: 1000, now: 3000 }), 'done');
  assert.equal(petMood('idle', { doneAt: 1000, idleSince: 1000, now: 10_000 }), 'idle');
  assert.equal(petMood('idle', { idleSince: 1000, now: 70_000 }), 'sleep');
});

test('每一帧都在画布里、没有重复的格子', () => {
  for (const [mood, [count]] of Object.entries(PET_FRAMES)) {
    for (let f = 0; f < count; f++) {
      const px = petPixels(mood, f);
      const keys = new Set(px.map(([x, y]) => `${x},${y}`));
      assert.equal(keys.size, px.length, mood);
      assert.ok(px.every(([x, y, c]) => x >= 0 && x < PET_W && y >= 0 && y < PET_H && 'oblfwpc'.includes(c)), mood);
    }
  }
  // 睁眼才有眼珠，睡觉时没有
  assert.ok(petPixels('idle').some(([, , c]) => c === 'p'));
  assert.ok(!petPixels('sleep').some(([, , c]) => c === 'p'));
});
