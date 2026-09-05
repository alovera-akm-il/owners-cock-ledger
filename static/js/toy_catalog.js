// Shared by toy_catalog.html (keyholder) and submissive_toys.html
// (submissive) — Duplication Ledger #2, docs/17-duplication-ledger.md
// §3. The two pages are the same toy-inventory feature with the same
// form and the same card-rendering shape; genuine per-role
// differences (which endpoints to hit, whether size/storage/usage
// notes show on the card, retire-instantly vs. request-removal) are
// read from data attributes on <main> or branched on IS_KEYHOLDER
// rather than duplicated wholesale.

const IS_KEYHOLDER = $('main').data('role') === 'keyholder';
const LIST_ENDPOINT = $('main').data('list-endpoint');
const DEVICES_ENDPOINT = $('main').data('devices-endpoint');
const LIMIT_RATINGS_ENDPOINT = $('main').data('limit-ratings-endpoint');

// Advisory, non-blocking cross-check against this submissive's
// hard/soft-rated structured limits (06-future-extensions.md §9) —
// never a hard block, the Keyholder remains the final authority.
// Keyholder-only: submissive_toys.html has no #limit-warning element
// and no rated-terms endpoint to check against.
let hardSoftRatedTerms = null;
function loadRatedTerms() {
  if (!LIMIT_RATINGS_ENDPOINT) return;
  apiCall('GET', LIMIT_RATINGS_ENDPOINT).done(function (list) {
    hardSoftRatedTerms = list
      .filter(function (i) { return i.rating === 'hard' || i.rating === 'soft'; })
      .map(function (i) { return { term: (i.category + ' ' + i.label).toLowerCase(), rating: i.rating, label: i.label }; });
  });
}
loadRatedTerms();

$('#f-category').on('input', function () {
  const value = $(this).val().trim().toLowerCase();
  $('#limit-warning').addClass('hidden');
  if (!value || !hardSoftRatedTerms) return;
  const hit = hardSoftRatedTerms.find(function (t) { return t.term.indexOf(value) !== -1 || value.indexOf(t.label.toLowerCase()) !== -1; });
  if (hit) {
    $('#limit-warning').text('Heads up: this touches a listed ' + hit.rating + ' limit ("' + hit.label + '"). Not a block — just worth checking in about.').removeClass('hidden');
  }
});

let deviceNamesById = {};
function loadDevices() {
  return apiCall('GET', DEVICES_ENDPOINT).done(function (devices) {
    const $select = $('#f-compatible-device');
    devices.forEach(function (d) { deviceNamesById[d.id] = d.name; });
    devices.filter(function (d) { return !d.retired_at; }).forEach(function (d) {
      $select.append($('<option>').val(d.id).text(d.name));
    });
  });
}

function resetForm() {
  $('#edit-toy-id').val('');
  $('#form-heading').text('Add toy');
  $('#form-message').addClass('hidden');
  $('#limit-warning').addClass('hidden');
  ['name', 'category', 'material', 'brand', 'color', 'size-notes', 'storage-location', 'care-instructions', 'usage-notes', 'tags', 'compatible-device', 'acquired-at']
    .forEach(function (f) { $('#f-' + f).val(''); });
  $('#photo-section').addClass('hidden');
  $('#photo-save-first-note').removeClass('hidden');
  $('#photo-preview').addClass('hidden').attr('src', '');
  $('#remove-photo-btn').addClass('hidden');
  $('#photo-message').addClass('hidden');
  $('#f-photo').val('');
}

$('#new-btn').on('click', function () {
  resetForm();
  $('#new-form').removeClass('hidden').addClass('panel-fade')[0].scrollIntoView({ behavior: 'smooth', block: 'center' });
});
$('#cancel-new').on('click', function () { $('#new-form').addClass('hidden'); });

function openEditForm(t) {
  $('#edit-toy-id').val(t.id);
  $('#form-heading').text('Editing toy');
  $('#form-message').addClass('hidden');
  $('#f-name').val(t.name || '');
  $('#f-category').val(t.category || '');
  $('#f-material').val(t.material || '');
  $('#f-brand').val(t.brand || '');
  $('#f-color').val(t.color || '');
  $('#f-size-notes').val(t.size_notes || '');
  $('#f-storage-location').val(t.storage_location || '');
  $('#f-care-instructions').val(t.care_instructions || '');
  $('#f-usage-notes').val(t.usage_notes || '');
  $('#f-tags').val((t.tags || []).join(', '));
  $('#f-compatible-device').val(t.compatible_device_id || '');
  $('#f-acquired-at').val(t.acquired_at ? t.acquired_at.slice(0, 10) : '');
  $('#photo-save-first-note').addClass('hidden');
  $('#photo-section').removeClass('hidden');
  $('#photo-message').addClass('hidden');
  $('#f-photo').val('');
  if (t.photo_url) {
    $('#photo-preview').attr('src', t.photo_url + '?t=' + Date.now()).removeClass('hidden');
    $('#remove-photo-btn').removeClass('hidden');
  } else {
    $('#photo-preview').addClass('hidden').attr('src', '');
    $('#remove-photo-btn').addClass('hidden');
  }
  $('#new-form').removeClass('hidden').addClass('panel-fade')[0].scrollIntoView({ behavior: 'smooth', block: 'center' });
}

function showPhotoMsg(text, ok) {
  const cls = ok
    ? 'bg-emerald-600/10 border border-emerald-600/30 text-emerald-300'
    : 'bg-red-600/10 border border-red-600/30 text-red-300';
  $('#photo-message').attr('class', 'text-xs mt-1.5 rounded-lg px-3 py-2 ' + cls).removeClass('hidden').text(text);
}

$('#f-photo').on('change', function () {
  const file = this.files[0];
  const toyId = $('#edit-toy-id').val();
  if (!file || !toyId) return;
  const formData = new FormData();
  formData.append('photo', file);
  $.ajax({
    url: '/api/v1/toys/' + toyId + '/photo',
    method: 'POST',
    data: formData,
    processData: false,
    contentType: false,
    headers: { 'X-CSRF-Token': getCookie('ocl_csrf') || '' },
  }).done(function (t) {
    $('#photo-preview').attr('src', t.photo_url + '?t=' + Date.now()).removeClass('hidden');
    $('#remove-photo-btn').removeClass('hidden');
    showPhotoMsg('Photo saved.', true);
    loadToys();
  }).fail(function (xhr) {
    showPhotoMsg(errorMessage(xhr, 'Could not upload that photo.'), false);
  }).always(function () { $('#f-photo').val(''); });
});

$('#remove-photo-btn').on('click', function () {
  const toyId = $('#edit-toy-id').val();
  if (!toyId) return;
  apiCall('DELETE', '/api/v1/toys/' + toyId + '/photo').done(function () {
    $('#photo-preview').addClass('hidden').attr('src', '');
    $('#remove-photo-btn').addClass('hidden');
    loadToys();
  });
});

$('#save-toy-btn').on('click', function () {
  const id = $('#edit-toy-id').val();
  const name = $('#f-name').val();
  if (!name) {
    $('#form-message').attr('class', 'rounded-lg px-3 py-2 text-xs bg-red-600/10 border border-red-600/30 text-red-300').text('Name is required.').removeClass('hidden');
    return;
  }
  const tags = $('#f-tags').val().split(',').map(function (s) { return s.trim(); }).filter(Boolean);
  const payload = {
    name: name,
    category: $('#f-category').val() || null,
    material: $('#f-material').val() || null,
    brand: $('#f-brand').val() || null,
    color: $('#f-color').val() || null,
    size_notes: $('#f-size-notes').val() || null,
    storage_location: $('#f-storage-location').val() || null,
    care_instructions: $('#f-care-instructions').val() || null,
    usage_notes: $('#f-usage-notes').val() || null,
    tags: tags.length ? tags : null,
    compatible_device_id: $('#f-compatible-device').val() || null,
    acquired_at: $('#f-acquired-at').val() ? $('#f-acquired-at').val() + 'T00:00:00Z' : null,
  };
  const request = id
    ? apiCall('PATCH', '/api/v1/toys/' + id, payload)
    : apiCall('POST', LIST_ENDPOINT, payload);
  request.done(function (savedOrNothing) {
    if (!id && savedOrNothing && savedOrNothing.id) {
      // Stay on the form after a first save so a photo can be added
      // right away, instead of a full-catalog reload losing context.
      openEditForm(savedOrNothing);
      loadToys();
      return;
    }
    $('#new-form').addClass('hidden');
    loadToys();
  }).fail(function (xhr) {
    $('#form-message').attr('class', 'rounded-lg px-3 py-2 text-xs bg-red-600/10 border border-red-600/30 text-red-300').text(errorMessage(xhr, 'Could not save toy.')).removeClass('hidden');
  });
});

function toyCard(t) {
  const $card = $('<div>').addClass('rounded-xl border overflow-hidden ' + (t.retirement_requested_at ? 'border-amber-700/40 bg-amber-500/5' : 'border-slate-800 bg-slate-900') + (t.retired_at ? ' opacity-50' : ''));
  if (t.photo_url) {
    $card.append($('<img class="h-40 w-full object-cover">').attr('src', t.photo_url).attr('alt', t.name));
  }
  const $body = $('<div class="p-4">');
  const $head = $('<div class="flex items-start justify-between">');
  $head.append($('<p class="text-sm font-medium">').text(t.name));
  $body.append($head);
  const meta = [t.category, t.material, t.brand].filter(Boolean).join(' · ');
  if (meta) $body.append($('<p class="text-xs text-slate-500 mt-0.5">').text(meta));
  if (IS_KEYHOLDER) {
    if (t.size_notes) $body.append($('<p class="text-xs text-slate-500">').text(t.size_notes));
    if (t.storage_location) $body.append($('<p class="text-xs text-slate-600">').text('Stored: ' + t.storage_location));
  }
  if (t.compatible_device_id) $body.append($('<p class="text-xs text-slate-600">').text('Compatible with: ' + (deviceNamesById[t.compatible_device_id] || 'unknown device')));
  if (t.acquired_at) $body.append($('<p class="text-xs text-slate-600">').text('Acquired: ' + t.acquired_at.slice(0, 10)));
  if (IS_KEYHOLDER && t.usage_notes) $body.append($('<p class="text-xs text-slate-600">').text('Usage: ' + t.usage_notes));
  if (t.tags && t.tags.length) {
    const $tags = $('<div class="flex flex-wrap gap-1 mt-2">');
    t.tags.forEach(function (tag) { $tags.append($('<span class="text-[10px] bg-slate-800 text-slate-400 px-1.5 py-0.5 rounded-full">').text(tag)); });
    $body.append($tags);
  }
  if (t.retired_at) {
    $body.append($('<span class="inline-block mt-2 text-[10px] font-semibold text-slate-400 bg-slate-800 px-2 py-0.5 rounded-full">').text('retired'));
  } else if (t.retirement_requested_at) {
    $body.append($('<span class="inline-block mt-2 text-[10px] font-semibold text-amber-400 bg-amber-500/10 px-2 py-0.5 rounded-full">').text(IS_KEYHOLDER ? 'removal requested' : 'removal requested · awaiting your Keyholder'));
    if (IS_KEYHOLDER) {
      const $actions = $('<div class="flex gap-2 mt-3">');
      const $decline = $('<button class="flex-1 text-xs font-medium text-slate-300 border border-slate-700 hover:border-slate-600 hover:bg-slate-800 rounded-lg px-2 py-1.5">').text('Decline');
      $decline.on('click', function () {
        apiCall('POST', '/api/v1/keyholder/toys/' + t.id + '/decline-removal').done(loadToys);
      });
      const $approve = $('<button class="flex-1 text-xs font-semibold text-slate-950 bg-amber-500 hover:bg-amber-400 rounded-lg px-2 py-1.5">').text('Approve & retire');
      $approve.on('click', function () {
        apiCall('POST', '/api/v1/keyholder/toys/' + t.id + '/retire').done(loadToys);
      });
      $actions.append($decline, $approve);
      $body.append($actions);
    }
  } else {
    const $actions = $('<div class="flex gap-2 mt-3">');
    const $edit = $('<button class="flex-1 text-xs font-medium text-slate-300 border border-slate-700 hover:border-slate-600 hover:bg-slate-800 rounded-lg px-2 py-1.5">').text('Edit');
    $edit.on('click', function () { openEditForm(t); });
    $actions.append($edit);
    if (IS_KEYHOLDER) {
      const $retire = $('<button class="text-xs font-medium text-red-400 border border-red-800/60 hover:bg-red-500/10 rounded-lg px-2 py-1.5">').text('Retire');
      $retire.on('click', function () {
        if (!confirm('Retire "' + t.name + '"? It stays visible in past history but drops out of the active catalog.')) return;
        apiCall('POST', '/api/v1/keyholder/toys/' + t.id + '/retire').done(loadToys);
      });
      $actions.append($retire);
    } else {
      const $request = $('<button class="text-xs font-medium text-slate-400 border border-slate-700 hover:border-slate-600 rounded-lg px-2 py-1.5">').text('Request removal');
      $request.on('click', function () {
        apiCall('POST', '/api/v1/submissive/toys/' + t.id + '/request-removal').done(loadToys);
      });
      $actions.append($request);
    }
    $body.append($actions);
  }
  $card.append($body);
  return $card;
}

function loadToys() {
  const includeRetired = $('#show-retired').is(':checked');
  apiCall('GET', LIST_ENDPOINT + (includeRetired ? '?include_retired=true' : '')).done(function (list) {
    const $grid = $('#toy-grid').empty();
    if (list.length === 0) {
      $grid.append($('<p class="text-sm text-slate-500 col-span-full">').text(IS_KEYHOLDER ? 'No toys in the catalog yet.' : 'No toys in your catalog yet.'));
      return;
    }
    list.forEach(function (t) { $grid.append(toyCard(t)); });
  });
}
$('#show-retired').on('change', loadToys);
loadDevices().always(loadToys);
