# Writes a PSB v2 with one synthetic red tile to stdout.
raw = ARGV.delete('--bgra')
abort 'usage: generate-emote-fixture.rb [--bgra]' unless ARGV.empty?
texture = {
  'texture' => {'type' => (raw ? 'RGBA8' : 'DXT5'), 'width' => 4, 'height' => 4,
                'truncated_width' => 4, 'truncated_height' => 4, 'pixel' => :red},
  'icon' => {'tile' => {'left' => 0, 'top' => 0, 'width' => 4, 'height' => 4,
                        'originX' => 0, 'originY' => 0, 'attr' => 0}}
}
motion = {'lastTime' => 1, 'loopTime' => 0, 'layer' => [
  {'label' => 'red', 'frameList' => [{'time' => 0, 'content' => {'src' => 'atlas', 'icon' => 'tile'}}]}
]}
root = {'id' => 'motion', 'source' => {'atlas' => texture},
        'object' => {'chara' => {'motion' => {'main' => motion}}},
        'metadata' => {'base' => {'chara' => 'chara', 'motion' => 'main'}}}
if raw
  root['spec'] = 'win'
  motion['priority'] = [{'time' => 0, 'content' => [0]}]
  motion['layer'][0]['type'] = 0
  motion['layer'][0]['frameList'][0]['type'] = 2
  texture['type'] = 0
end

def array(values)
  [0x0e, values.length, 0x0e, *values].pack('CvCv*')
end

names = []
strings = []
index = ->(table, value) { table.index(value) || table.push(value).length - 1 }
with_offsets = lambda do |entries|
  offset = 0
  offsets = entries.map { |entry| previous = offset; offset += entry.bytesize; previous }
  array(offsets) + entries.join
end
encode = lambda do |value|
  case value
  when Hash
    [0x21].pack('C') + array(value.keys.map { |key| index.call(names, key) }) +
      with_offsets.call(value.values.map { |entry| encode.call(entry) })
  when Array then [0x20].pack('C') + with_offsets.call(value.map { |entry| encode.call(entry) })
  when String then [0x15, index.call(strings, value)].pack('CC')
  when Integer then [0x05, value].pack('Cc')
  when :red then [0x19, 0].pack('CC')
  else raise "unsupported fixture value: #{value.inspect}"
  end
end
entries = encode.call(root)

trie = {}
names.each_with_index do |name, i|
  leaf = name.bytes.reduce(trie) { |node, byte| node[byte] ||= {} }
  leaf[0] = i
end
bases = Hash.new(0)
parents = {0 => 0}
terminals = Array.new(names.length)
queue = [[0, trie]]
until queue.empty?
  parent, children = queue.shift
  base = 0
  base += 1 while children.keys.any? { |byte| parents.key?(base + byte) }
  bases[parent] = base
  children.each do |byte, child|
    node = base + byte
    parents[node] = parent
    byte.zero? ? terminals[child] = node : queue.push([node, child])
  end
end
name_data = array((0..bases.keys.max).map { |i| bases[i] }) +
            array((0..parents.keys.max).map { |i| parents.fetch(i, 0) }) + array(terminals)
string_data = strings.map { |string| string + "\0" }
string_table = with_offsets.call(string_data).byteslice(0, 4 + 2 * strings.length)
pixel = if raw
          [0, 0, 255, 255].pack('C*') * 16
        else
          [255, 0, 0, 0, 0, 0, 0, 0, 0, 248, 0, 0, 0, 0, 0, 0].pack('C*')
        end
sections = [name_data, string_table, string_data.join, array([0]), array([pixel.bytesize]), pixel, entries]
offset = 40
offsets = sections.map { |data| previous = offset; offset += data.bytesize; previous }
STDOUT.binmode
STDOUT.write("PSB\0" + [2, 0, 40, *offsets].pack('vvV*') + sections.join)
