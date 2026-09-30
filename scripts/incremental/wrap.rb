input, output, width, height, columns, rows = ARGV
abort 'Usage: wrap.rb ANNEX_B OUTPUT WIDTH HEIGHT [COLUMNS] [ROWS]' unless input && output && width && height
width = Integer(width)
height = Integer(height)
columns = Integer(columns || '1')
rows = Integer(rows || '1')
raise 'Invalid fixture dimensions' unless width.positive? && height.positive? && (1..256).cover?(columns) && (1..256).cover?(rows)
nals = File.binread(input).split(/\x00\x00\x00?\x01/n).reject(&:empty?)
parameters = (32..34).map do |kind|
  nals.find { |nal| (nal.getbyte(0) >> 1 & 63) == kind } || raise('Missing parameter set')
end
pictures = nals.select { |nal| [19, 20].include?(nal.getbyte(0) >> 1 & 63) }
raise 'One IDR required' unless pictures.size == 1
payload = pictures.map { |nal| [nal.bytesize].pack('N') + nal }.join
sps = parameters[1].byteslice(2..).gsub(/\x00\x00\x03/n, "\x00\x00".b)
hvcc = [1].pack('C') + sps.byteslice(1, 12) + [0xf0, 0, 0xfc, 0xfd, 0xf8, 0xf8, 0, 0, 0x0f, 3].pack('C*')
parameters.each_with_index { |nal, i| hvcc += [0x80 + 32 + i, 1, nal.bytesize].pack('Cnn') + nal }
def box(kind, data)
  [data.bytesize + 8].pack('N') + kind + data
end

def full(kind, version, data)
  box(kind, [version << 24].pack('N') + data)
end

count = columns * rows
grid = count > 1
total = count + (grid ? 1 : 0)
primary = grid ? total : 1
ispe = ->(w, h) { full('ispe', 0, [w, h].pack('NN')) }
properties = [box('hvcC', hvcc), ispe.call(width, height), full('pixi', 0, [3, 8, 8, 8].pack('C*'))]
properties << ispe.call(width * columns, height * rows) if grid
extra = []
if ENV['CROP']
  w, h = ENV.fetch('CROP').split('x').map { |n| Integer(n) }
  properties << box('clap', [w, 1, h, 1, 0, 1, 0, 1].pack('N*'))
  extra << properties.length
end
if ENV['ROTATION']
  properties << box('irot', [Integer(ENV.fetch('ROTATION'))].pack('C'))
  extra << properties.length
end
if ENV['MIRROR']
  properties << box('imir', [Integer(ENV.fetch('MIRROR'))].pack('C'))
  extra << properties.length
end
items = (1..total).map do |id|
  kind = id == primary && grid ? 'grid' : 'hvc1'
  full('infe', 2, [id, 0].pack('nn') + kind + "\0")
end.join
ipma = [total].pack('N') + (1..total).map do |id|
  associations = id == primary && grid ? [0x84] : [0x81, 2, 3]
  associations += extra.map { |n| n | 0x80 } if id == primary
  [id, associations.length].pack('nC') + associations.pack('C*')
end.join
references = grid ? full('iref', 0, box('dimg', [primary, count, *1..count].pack('n*'))) : ''.b
griddata = grid ? [0, 1, rows - 1, columns - 1, width * columns, height * rows].pack('CCCCNN') : ''.b
ftyp = box('ftyp', "heic\0\0\0\0mif1heic".b)
make_meta = lambda do |offset|
  locations = [0x44, 0, total].pack('CCn')
  (1..total).each do |id|
    length = id == primary && grid ? griddata.bytesize : payload.bytesize
    locations += [id, 0, 1].pack('nnn') + [offset, length].pack('NN')
    offset += length
  end
  full('meta', 0,
       full('hdlr', 0, "\0\0\0\0pict".b + "\0" * 13) +
       full('pitm', 0, [primary].pack('n')) +
       full('iinf', 0, [total].pack('n') + items) +
       full('iloc', 0, locations) +
       box('iprp', box('ipco', properties.join) + full('ipma', 0, ipma)) + references)
end
meta = make_meta.call(0)
meta = make_meta.call(ftyp.bytesize + meta.bytesize + 8)
File.binwrite(output, ftyp + meta + box('mdat', payload * count + griddata))
puts "#{output}: #{width * columns}x#{height * rows}, #{count} coded items"
